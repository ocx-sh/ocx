// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Client of an `index.ocx.sh`-style static-file pointer index, not a registry
//! (`adr_index_indirection.md` Decision F, `adr_oci_index_only_dispatch.md`):
//! ```text
//! logical id (ocx.sh/<ns>/<pkg>[:tag])
//!   → GET /p/<ns>/<pkg>.json                root  → tags[tag].content = image-index digest
//!   → GET /p/<ns>/<pkg>/o/sha256/<hex>.json index → VERIFY sha256(bytes)==hex
//!                                                 → manifests[] = OCI descriptors
//!   → select_best(host, platforms)          → leaf platform-manifest digest
//!   → root.repository (oci://…)             → physical fetch through the mirror seam
//! ```
//!
//! The root is trusted on its channel (TLS, or the operator's filesystem for `file://`);
//! everything below it is verified, a digest mismatch being a hard
//! [`DataError`](ocx_exit::ExitCode::DataError). Publisher-controlled `annotations` and
//! `artifactType` are stored, never rendered.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;

use super::error::Result;
use super::wire::{CatalogDocument, CatalogIndex, IndexFormatConfig, IndexRoot, RootTag, gate_format_version};
use super::{IndexOperation, error, index_impl};
use ocx_oci::client::ReadAddressing;
use ocx_oci::transport_policy::{self, Attempt, RetryBudget, RetryPolicy, TransportHardening};

use ocx_util::fs::path::{FileReference, Spelling};
use ocx_util::singleflight::{self, Acquisition};

use ocx_oci::client::MAX_INDEX_DOCUMENT_BYTES;

/// Connect timeout for an index fetch, so a dead endpoint cannot stall a resolve (CWE-400).
const INDEX_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Per-frame idle bound for an index fetch (CWE-400, `adr_index_sync_performance.md`): idle
/// rather than total, so an honest slow body over a throttled link never fails.
const INDEX_IDLE_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Per-attempt outer cap: without it a peer dribbling a byte every 29 s never trips
/// [`INDEX_IDLE_BOUND`] and holds its [`MAX_INDEX_DOCUMENT_BYTES`] buffer, ×512 in flight, forever.
/// A backstop, not an SLA: it must not fire on any honest transfer.
const INDEX_OUTER_CAP: std::time::Duration = std::time::Duration::from_secs(300);

/// Redacts `user[:password]@` userinfo from `url`'s authority before it reaches an error or
/// log line (CWE-532); a URL with no `://` is returned untouched.
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

/// Transport for the static-file index endpoints: [`ReqwestIndexTransport`] for `https://`
/// (or gated `http://`), [`FileIndexTransport`](super::FileIndexTransport) for `file://`.
#[async_trait]
pub trait IndexTransport: Send + Sync {
    /// Fetch `url` unconditionally: bytes, [`IndexFetch::NotFound`], or an error, never a
    /// not-modified outcome.
    async fn get(&self, url: &str) -> Result<IndexFetch>;

    /// [`Self::get`] with every HTTP cache on the path told to revalidate, for a read a removal
    /// decision rests on. The default is `get`: a transport with no HTTP layer has no cache.
    async fn get_uncached(&self, url: &str) -> Result<IndexFetch> {
        self.get(url).await
    }

    fn box_clone(&self) -> Box<dyn IndexTransport>;
}

/// Whether an HTTP cache on the path may answer a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freshness {
    Cacheable,
    Revalidate,
}

impl Clone for Box<dyn IndexTransport> {
    fn clone(&self) -> Self {
        self.box_clone()
    }
}

/// `reqwest`-backed production [`IndexTransport`], on rustls like the `oci-client` fork.
///
/// Every clone shares one [`RetryBudget`]; per-clone counters would let each package of the
/// sync fan-out meter its own budget.
#[derive(Clone)]
pub struct ReqwestIndexTransport {
    /// Built on first request and shared by every clone: building parses the bundled roots
    /// (~38 ms), which eager building charged to every online invocation, trampolines included.
    client: std::sync::Arc<std::sync::OnceLock<reqwest::Client>>,
    hardening: TransportHardening,
    policy: RetryPolicy,
    budget: RetryBudget,
    /// Operator-supplied extra CA roots, set via [`ReqwestIndexTransport::with_extra_roots`].
    extra_roots: ocx_util::tls::ExtraRoots,
}

/// Builds the index HTTP client under `hardening`'s bounds with the bundled Mozilla roots plus
/// `extra_roots`; seeding roots keeps reqwest off the OS trust store, whose verifier panics on a
/// host with none (minimal container / CI runner).
fn build_index_http_client(hardening: &TransportHardening, extra_roots: &ocx_util::tls::ExtraRoots) -> reqwest::Client {
    // Every returned client goes through this: without `Policy::none()` a 3xx could move the
    // fetch to http:// or an internal host after `resolve_base_url`'s plain-HTTP gate (CWE-918).
    let harden = |builder: reqwest::ClientBuilder| {
        builder
            .connect_timeout(hardening.connect_timeout)
            .read_timeout(hardening.idle_bound)
            .timeout(hardening.outer_cap)
            .redirect(reqwest::redirect::Policy::none())
    };
    let builder = extra_roots.seed(ocx_util::tls::seed_embedded_roots(harden(reqwest::Client::builder())));
    builder.build().unwrap_or_else(|error| {
        // Never drops a configured root: every `ExtraRoots` already survived `parse_pem`'s probe build.
        log::warn!("index HTTP client build with bundled roots failed ({error}); using hardened reqwest defaults");
        // No bare-client third arm: it has no timeouts and would follow redirects past the plain-HTTP gate.
        harden(reqwest::Client::builder())
            .build()
            .expect("a client with no custom roots and only timeout and redirect settings always builds")
    })
}

/// Test seam replacing the three index timeouts, as `connect,idle,outer` milliseconds, so the
/// acceptance row for the shipped bounds need not out-wait minutes in real time.
#[cfg(any(test, feature = "__testing"))]
const TESTING_TIMEOUTS_ENV: &ocx_env::EnvVar = &ocx_env::__OCX_TESTING_INDEX_TIMEOUTS_MS;

/// [`TESTING_TIMEOUTS_ENV`] parsed, or `None` when it is unset.
///
/// **Panics** on a malformed value: silently falling back would let a typo'd test out-wait the
/// real deadline and still pass.
#[cfg(any(test, feature = "__testing"))]
fn testing_hardening_override() -> Option<TransportHardening> {
    let raw = TESTING_TIMEOUTS_ENV.get_raw()?.into_string().ok()?;
    let name = TESTING_TIMEOUTS_ENV.name;
    let millis: Vec<u64> = raw
        .split(',')
        .map(|field| {
            field
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("{name} must be `connect,idle,outer` milliseconds, got {raw:?}"))
        })
        .collect();
    let [connect, idle, outer] = millis[..] else {
        panic!("{name} must name exactly three milliseconds values, got {raw:?}")
    };
    Some(TransportHardening {
        connect_timeout: std::time::Duration::from_millis(connect),
        idle_bound: std::time::Duration::from_millis(idle),
        outer_cap: std::time::Duration::from_millis(outer),
    })
}

#[cfg(not(any(test, feature = "__testing")))]
fn testing_hardening_override() -> Option<TransportHardening> {
    None
}

impl ReqwestIndexTransport {
    pub fn new() -> Self {
        let shipped = TransportHardening {
            connect_timeout: INDEX_CONNECT_TIMEOUT,
            idle_bound: INDEX_IDLE_BOUND,
            outer_cap: INDEX_OUTER_CAP,
        };
        Self::with_hardening(&testing_hardening_override().unwrap_or(shipped), RetryPolicy::default())
    }

    /// Adds operator-supplied CA roots on top of the bundled Mozilla set.
    ///
    /// **Must be called before the first request**: the client is built once, on first use, and
    /// keeps whatever roots it was built with.
    pub fn with_extra_roots(mut self, extra_roots: ocx_util::tls::ExtraRoots) -> Self {
        debug_assert!(
            self.client.get().is_none(),
            "with_extra_roots after the client was built"
        );
        self.extra_roots = extra_roots;
        self
    }

    /// Construction with the bounds injected, so a fixture can test them in milliseconds.
    fn with_hardening(hardening: &TransportHardening, policy: RetryPolicy) -> Self {
        Self {
            client: std::sync::Arc::new(std::sync::OnceLock::new()),
            hardening: *hardening,
            policy,
            budget: RetryBudget::new(),
            extra_roots: ocx_util::tls::ExtraRoots::default(),
        }
    }

    fn client(&self) -> &reqwest::Client {
        self.client
            .get_or_init(|| build_index_http_client(&self.hardening, &self.extra_roots))
    }

    /// One attempt at `url`, from dispatch through the last body byte. A retryable outcome
    /// carries its terminal value, so giving up never changes the error the caller sees.
    async fn attempt(client: &reqwest::Client, url: &str, freshness: Freshness) -> Attempt<Result<IndexFetch>> {
        let mut request = client.get(url);
        if freshness == Freshness::Revalidate {
            // `Pragma` for HTTP/1.0 intermediaries that ignore `Cache-Control` (RFC 9111 §5.4).
            request = request
                .header(reqwest::header::CACHE_CONTROL, "no-cache")
                .header(reqwest::header::PRAGMA, "no-cache");
        }
        let mut response = match request.send().await {
            Ok(response) => response,
            Err(source) => return Self::transport_failure(url, None, source),
        };

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Attempt::Done(Ok(IndexFetch::NotFound));
        }
        // Everything else, a `304` included, is an error: a 404 is memoized as absence, so one
        // misbehaving edge must not decide a name for the rest of the process.
        if !status.is_success() {
            // Read per attempt, never cached: ACR counts `Retry-After` down across polls.
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
            let retryable = matches!(
                &failure,
                super::error::Error::IndexHttpFailed { status: Some(code), .. }
                    if transport_policy::is_retryable_status(*code)
            );
            return if retryable {
                Attempt::Retry {
                    retry_after,
                    terminal: Err(failure),
                }
            } else {
                Attempt::Done(Err(failure))
            };
        }

        // Refuse a declared oversize body unread (CWE-400); not retried, the size will not change.
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
            }));
        }

        // Checked before each append (CWE-400): an omitted or lying Content-Length must not
        // stream past the cap, which, not the timeout, is what bounds memory.
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                // Retried as a whole `GET`, safe because every request on this path is idempotent.
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
                        }));
                    }
                    body.extend_from_slice(&chunk);
                }
            }
        }
        Attempt::Done(Ok(IndexFetch::Found { bytes: body }))
    }

    /// Wraps a `reqwest` failure, retryable only for the transient transport class; a refused
    /// certificate is terminal and carries [`transport_policy::UntrustedCertificateHint`].
    fn transport_failure(url: &str, status: Option<u16>, source: reqwest::Error) -> Attempt<Result<IndexFetch>> {
        let retryable = transport_policy::is_retryable_transport_error(&source);
        let source: Box<dyn std::error::Error + Send + Sync> = if transport_policy::is_tls_certificate_refusal(&source)
        {
            let url = source.url().cloned();
            Box::new(transport_policy::UntrustedCertificateHint::for_url(
                url.as_ref(),
                source,
            ))
        } else {
            Box::new(source)
        };
        let failure: Result<IndexFetch> = Err(super::error::Error::IndexHttpFailed {
            url: redact_url(url),
            status,
            source,
        });
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
    /// Fetches `url`, retrying transient failures under [`RetryPolicy`] and the run-global
    /// [`RetryBudget`], which bounds retry volume as `outer_cap` bounds each attempt.
    async fn get(&self, url: &str) -> Result<IndexFetch> {
        self.fetch(url, Freshness::Cacheable).await
    }

    async fn get_uncached(&self, url: &str) -> Result<IndexFetch> {
        self.fetch(url, Freshness::Revalidate).await
    }

    fn box_clone(&self) -> Box<dyn IndexTransport> {
        Box::new(self.clone())
    }
}

impl ReqwestIndexTransport {
    async fn fetch(&self, url: &str, freshness: Freshness) -> Result<IndexFetch> {
        // The in-process CLI seam's no-network guarantee; `false` outside tests.
        if ocx_oci::client::network_refused() {
            return Err(super::error::Error::IndexHttpFailed {
                url: redact_url(url),
                status: None,
                source: "network access refused".into(),
            });
        }
        let client = self.client();
        let policy = &self.policy;
        transport_policy::run(policy, &self.budget, move |attempt| async move {
            if attempt > 0 {
                // `debug!`, never `warn!`: a retried transient is a common
                // benign state, and an operator-facing warning per retry across
                // a 512-wide fan-out is noise, not signal. Redacted
                // because an index base URL may embed `user:password@`
                // (CWE-532) — same reason every error below this line is.
                log::debug!(
                    "retrying index request to {} (attempt {} of {})",
                    redact_url(url),
                    attempt + 1,
                    policy.attempts
                );
            }
            Self::attempt(client, url, freshness).await
        })
        .await
    }
}

// ── Source ───────────────────────────────────────────────────────────────────

/// A resolved index base URL paired with the transport for its scheme, decided once by
/// [`OcxIndex::resolve_base_url`] (`adr_servable_index_snapshot.md`).
pub struct IndexBase {
    /// Trailing-slash-trimmed, as [`OcxIndex::new`] stores it.
    pub url: String,
    pub transport: Box<dyn IndexTransport>,
}

impl std::fmt::Debug for IndexBase {
    // Hand-written so the URL, which may carry `user:password@`, goes through `redact_url` (CWE-532).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBase")
            .field("url", &redact_url(&self.url))
            .finish_non_exhaustive()
    }
}

/// The lowercased scheme of `url`, or `None` when it carries none.
fn scheme_of(url: &str) -> Option<String> {
    url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase())
}

/// The tail of a single-slash `file:` base (`file:/srv/x`), which has no `://` for
/// [`scheme_of`] to find; case-insensitive like [`scheme_of`].
fn file_colon_tail(url: &str) -> Option<&str> {
    let (prefix, tail) = url.split_at_checked("file:".len())?;
    (prefix.eq_ignore_ascii_case("file:") && !tail.starts_with("//")).then_some(tail)
}

/// The [`Error::InvalidIndexUrl`](error::Error::InvalidIndexUrl) `origin` for a single-slash
/// `file:` base, naming the correction. A relative tail gets the shape, not a literal:
/// `file://srv/x` would name an authority, and a guessed leading slash a directory never written.
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
    source: Option<ocx_config::mirror::MirrorConfigError>,
) -> error::Error {
    error::Error::InvalidIndexUrl {
        namespace: namespace.to_string(),
        // A base or mirror value may embed `user:password@` (CWE-532).
        url: redact_url(url),
        origin,
        source: source.map(Box::new),
    }
}

/// Resolves a `file://` base, requiring an empty authority (`file://host/…` is UNC, not local)
/// and an absolute path other than `/`. A bare drive (`file:///C:/`) is refused: Win32 resolves
/// `C:` against the per-drive working directory, serving the index from wherever `ocx` launched.
fn resolve_file_base(namespace: &str, base: &str) -> Result<IndexBase> {
    // `len() == 3` is a bare `/C:` drive, refused on every platform so validity is host-independent.
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

/// The absolute filesystem path a `file://` tail names. The `/C:/…` separator strip is gated on
/// Windows: elsewhere it would turn a Unix root named `/C:/…` into a relative path.
fn file_root(path: &str) -> std::path::PathBuf {
    if cfg!(windows) && has_drive_prefix(path) {
        std::path::PathBuf::from(&path[1..])
    } else {
        std::path::PathBuf::from(path)
    }
}

/// Whether `path` is a Windows drive-letter `file://` tail (`/C:/…`), on every OS.
fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':'
}

/// Per-invocation caches shared across [`OcxIndex`] clones; not the committed local index.
#[derive(Default)]
struct SourceCacheInner {
    /// repository → root, `None` for a confirmed 404, so an absent name costs one 404 per process.
    roots: BTreeMap<String, Option<CachedRoot>>,
    /// Set only for a served `config.json` that passed the version gate: an absent one is
    /// re-asked so a later-published config is seen, an unsupported one stays a hard error.
    config: Option<Arc<IndexFormatConfig>>,
}

/// One memoized root: the verbatim `p/<repo>.json` bytes beside their parse, so a first-sight
/// digest-addressed resolve pays one `GET` for both [`OcxIndex::resolve_root`] and
/// `fetch_root_document`.
#[derive(Clone)]
struct CachedRoot {
    /// Exactly as served; a re-serialisation of `parsed` would stop hashing to the catalog entry.
    bytes: Arc<Vec<u8>>,
    parsed: Arc<IndexRoot>,
}

/// Max keys a source's singleflight group admits, sized for a whole `ocx index sync`: the groups
/// live for the process with one key per repository, and a small cap exits `TempFail(75)` on
/// successes alone. A memory backstop, never a throughput limit.
const SOURCE_SINGLEFLIGHT_MAX_KEYS: usize = 1 << 20;

/// How long a coalesced caller waits for the leader: four [`INDEX_OUTER_CAP`]s cover three
/// attempts plus backoff, and a shorter wait turns one slow link into `TempFail(75)` for waiters.
const SOURCE_SINGLEFLIGHT_TIMEOUT: Duration = Duration::from_secs(INDEX_OUTER_CAP.as_secs() * 4);

/// A live `index.ocx.sh`-style source.
#[derive(Clone)]
pub struct OcxIndex {
    transport: Box<dyn IndexTransport>,
    /// Static-file base URL, `[mirrors]` index override applied, trailing slash trimmed.
    base_url: String,
    /// The logical registry this source serves (e.g. `"ocx.sh"`).
    namespace: String,
    /// Physical-fetch client; its `GuardedResolver` pins the address `physical_identifier` validated.
    client: ocx_oci::Client,
    /// When false, a tag resolving to a yanked entry is refused.
    allow_yanked: bool,
    /// SSRF escape hatch (`[registries."<ns>"].trusted_hosts`).
    trusted_hosts: Vec<String>,
    /// Plain-HTTP authorities, the same list `client` carries; read only to pick the dial scheme.
    insecure_hosts: Vec<String>,
    /// Proxy-route rules for the SSRF pre-flight in [`Self::physical_identifier`].
    proxy_rules: Arc<ocx_oci::ssrf::ProxyRules>,
    cache: Arc<RwLock<SourceCacheInner>>,
    /// Coalesces cold `config.json` misses; broadcasts only a served document.
    config_group: singleflight::Group<(), Option<Arc<IndexFormatConfig>>>,
    /// Coalesces cold `p/<repo>.json` misses, keyed like [`SourceCacheInner::roots`]. Shared across
    /// `box_clone` so coalescing reaches the fan-out; safe only because `OcxIndex` writes nothing
    /// locally, unlike `ChainedIndex`, whose read-only views need fresh groups.
    root_group: singleflight::Group<String, Option<Arc<IndexRoot>>>,
}

/// Construction inputs for [`OcxIndex::new`].
pub struct OcxIndexConfig {
    pub transport: Box<dyn IndexTransport>,
    pub base_url: String,
    pub namespace: String,
    pub client: ocx_oci::Client,
    pub allow_yanked: bool,
    /// SSRF escape hatch (`[registries."<ns>"].trusted_hosts`); empty guards every host.
    pub trusted_hosts: Vec<String>,
    /// Plain-HTTP authorities, the same list `client` was built with; empty = all HTTPS.
    pub insecure_hosts: Vec<String>,
    /// Whether a physical dial is proxied; tests pass explicit rules so the verdict never
    /// depends on the developer's environment.
    pub proxy_rules: Arc<ocx_oci::ssrf::ProxyRules>,
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

    /// Whether `registry` is the one this source serves; no I/O.
    pub fn serves_registry(&self, registry: &str) -> bool {
        registry == self.namespace
    }

    /// [`Authoritative`](super::Jurisdiction::Authoritative) for this source's own registry,
    /// [`Outside`](super::Jurisdiction::Outside) otherwise, with no I/O. A name with no root is a
    /// hard miss, never a hand-off to the plain OCI registry, which would skip the yank gate.
    pub fn jurisdiction(&self, identifier: &ocx_oci::PackageRef) -> super::Jurisdiction {
        if self.serves_registry(identifier.registry()) {
            super::Jurisdiction::Authoritative
        } else {
            super::Jurisdiction::Outside
        }
    }

    /// The base URL this source reads, with any `user[:password]@` userinfo redacted, for a
    /// report or error message.
    pub fn redacted_base_url(&self) -> String {
        redact_url(&self.base_url)
    }

    /// This source's SSRF escape hatch ([`OcxIndexConfig::trusted_hosts`]).
    pub fn trusted_hosts(&self) -> &[String] {
        &self.trusted_hosts
    }

    /// The physical-fetch client ([`OcxIndexConfig::client`]).
    pub fn client(&self) -> &ocx_oci::Client {
        &self.client
    }

    /// The plain-HTTP authorities ([`OcxIndexConfig::insecure_hosts`]).
    pub fn insecure_hosts(&self) -> &[String] {
        &self.insecure_hosts
    }

    /// Resolves the static-file base for `namespace`: `[registries."<ns>"] index`, else the
    /// default, then the `[mirrors."<host>"] index` override (replace, no fallback).
    ///
    /// Schemes: `https` or none; `http` only for an `insecure_hosts` host (CWE-319); `file` only
    /// on the configured base.
    ///
    /// # Errors
    ///
    /// [`Error::PlainHttpIndexNotAllowed`](super::error::Error::PlainHttpIndexNotAllowed)
    /// for an ungated `http://` target; [`Error::InvalidIndexUrl`](super::error::Error::InvalidIndexUrl)
    /// for an unparseable base, another scheme, or a `file://` base with an authority or a
    /// relative path.
    pub fn resolve_base_url(
        config: &ocx_config::Config,
        namespace: &str,
        mirrors_index: &BTreeMap<String, ocx_oci::client::mirror_map::ParsedMirror>,
        insecure_hosts: &[String],
        extra_roots: &ocx_util::tls::ExtraRoots,
    ) -> Result<IndexBase> {
        let base = config
            .registries
            .as_ref()
            .and_then(|table| table.get(namespace))
            .and_then(|entry| entry.index.as_deref())
            .filter(|url| !url.is_empty())
            .unwrap_or(ocx_config::index::DEFAULT_INDEX_BASE_URL);

        // Check 1a: `file:/srv/x` has no `://`, so without this it passes as an https default
        // and fails much later as a DNS lookup for a host named `file`.
        if let Some(tail) = file_colon_tail(base) {
            return Err(invalid_index_url(namespace, base, file_colon_origin(tail), None));
        }

        // Check 1, before `parse_url`, which reads a `file` base's empty authority as `MissingHost`.
        // Only the `FileUrl` spelling: a schemeless base means https, never a filesystem path.
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

        // The mirror URL parser, so the plain-HTTP gate matches the registry role byte for byte.
        let parsed = ocx_config::mirror::parse_url(base).map_err(|source| {
            invalid_index_url(
                namespace,
                base,
                error::INDEX_URL_FROM_REGISTRIES.to_string(),
                Some(source),
            )
        })?;

        // `upstream` is kept to name the offending `[mirrors]` entry if check 2 refuses.
        let upstream = parsed.host.clone();
        let overridden = mirrors_index.get(&upstream);
        let target = overridden.cloned().unwrap_or(parsed);
        let path = if target.path_prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", target.path_prefix)
        };
        let url = format!("{}://{}{}", target.protocol, target.host, path);

        // Check 2, post-override: a `[mirrors]` entry (`OCX_MIRRORS` too) replaces the scheme,
        // bypassing check 1.
        match target.protocol.as_str() {
            "https" => {}
            "http" if ocx_oci::ssrf::allows_plain_http(insecure_hosts, &target.host) => {}
            "http" => {
                return Err(super::error::Error::PlainHttpIndexNotAllowed {
                    namespace: namespace.to_string(),
                    host: target.host,
                });
            }
            _ => {
                // Check 1 admitted only http/https, so this scheme came from the override.
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
            transport: Box::new(ReqwestIndexTransport::new().with_extra_roots(extra_roots.clone())),
        })
    }

    // ── config.json ──────────────────────────────────────────────────────────

    /// Resolves and version-gates this source's `config.json`, memoized once served.
    ///
    /// A 404 resolves to [`IndexFormatConfig::assumed_v1`] and is never memoized, so a tree
    /// that later publishes one is picked up without a restart.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat)
    /// on a served-but-unknown version; the transport error otherwise. Either may arrive inside
    /// a transparent [`Error::SourceFetchFailed`](super::error::Error::SourceFetchFailed).
    async fn check_format_version(&self) -> Result<Arc<IndexFormatConfig>> {
        if let Some(config) = &self.cache.read().await.config {
            return Ok(config.clone());
        }
        // Coalesced: under the sync fan-out every task misses together.
        let handle = match self
            .config_group
            .try_acquire(())
            .await
            .map_err(error::Error::SingleflightFailed)?
        {
            Acquisition::Leader(handle) => handle,
            Acquisition::Resolved(Some(config)) => return Ok(config),
            // A group entry lives for the process, so an assumed v1 asks the wire itself here,
            // or a later-published `config.json` would never be seen.
            Acquisition::Resolved(None) => return Ok(or_assumed_v1(self.fetch_format_config().await?)),
        };
        match self.fetch_format_config().await {
            Ok(served) => {
                // `None` makes each waiter derive its own assumed v1.
                handle.complete(served.clone());
                Ok(or_assumed_v1(served))
            }
            Err(error) => Err(error::broadcast_failure(handle, error)),
        }
    }

    /// One version-gated `GET config.json`, memoizing only a served document; `Ok(None)` on a 404.
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
                gate_format_version(IndexFormatConfig::assumed_v1().format_version)?;
                Ok(None)
            }
        }
    }

    // ── root (volatile) ──────────────────────────────────────────────────────

    /// Fetches and memoizes the root for `repository`; `Ok(None)` on a 404, memoized like a hit.
    ///
    /// # Errors
    ///
    /// [`Error::IndexHttpFailed`](super::error::Error::IndexHttpFailed) for any non-404 failure,
    /// possibly inside a transparent [`Error::SourceFetchFailed`](super::error::Error::SourceFetchFailed).
    /// A failure memoizes nothing, so a repeat ask re-requests.
    async fn resolve_root(&self, repository: &str) -> Result<Option<Arc<IndexRoot>>> {
        // The version gate runs before any root is consumed.
        self.check_format_version().await?;
        if let Some(cached) = self.cache.read().await.roots.get(repository) {
            return Ok(cached.as_ref().map(|cached| cached.parsed.clone()));
        }
        // Coalesced: the per-tag fan-out asks for one repository's root once per tag.
        let handle = match self
            .root_group
            .try_acquire(repository.to_string())
            .await
            .map_err(error::Error::SingleflightFailed)?
        {
            Acquisition::Leader(handle) => handle,
            // A hit and a confirmed miss are both answers, unlike an assumed v1.
            Acquisition::Resolved(root) => return Ok(root),
        };
        match self.fetch_root(repository, Freshness::Cacheable).await {
            Ok(cached) => {
                self.memoize_root(repository, cached.clone()).await;
                // Waiters get the parse; the bytes reach `fetch_root_document` through the memo.
                let parsed = cached.map(|cached| cached.parsed);
                handle.complete(parsed.clone());
                Ok(parsed)
            }
            Err(error) => Err(error::broadcast_failure(handle, error)),
        }
    }

    /// One `GET p/<repo>.json`; `Ok(None)` only on a confirmed 404.
    async fn fetch_root(&self, repository: &str, freshness: Freshness) -> Result<Option<CachedRoot>> {
        let url = format!("{}/p/{}.json", self.base_url, repository);
        let fetched = match freshness {
            Freshness::Cacheable => self.transport.get(&url).await?,
            Freshness::Revalidate => self.transport.get_uncached(&url).await?,
        };
        match fetched {
            IndexFetch::Found { bytes } => {
                let parsed: IndexRoot = parse_document(&bytes, &url)?;
                Ok(Some(CachedRoot {
                    bytes: Arc::new(bytes),
                    parsed: Arc::new(parsed),
                }))
            }
            IndexFetch::NotFound => Ok(None),
        }
    }

    /// Memoizes a root under the bare repository key, which has no registry component: only a
    /// call that actually asked *this* source may memoize, or a foreign identifier's `None`
    /// silently hides the served registry's same-named package for the rest of the process.
    async fn memoize_root(&self, repository: &str, root: Option<CachedRoot>) {
        self.cache.write().await.roots.insert(repository.to_string(), root);
    }

    // ── dispatch object (immutable, VERIFIED) ────────────────────────────────

    /// Fetches the dispatch object for `(repository, digest)`, verifies its bytes hash to
    /// `digest`, and returns them verbatim beside the parse, so the local copy keeps keys this
    /// client does not model. `Ok(None)` on a 404.
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
        digest: &ocx_oci::Digest,
    ) -> Result<Option<(Vec<u8>, ocx_oci::ImageIndex)>> {
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
            });
        }

        // Deserialisation proves shape only (`schemaVersion` is any `u8`), hence the validation;
        // `artifactType` is deliberately not gated on, as nothing in ocx reads it.
        let index: ocx_oci::ImageIndex = parse_document(&bytes, &url)?;
        ocx_oci::manifest::validate_image_index(&index).map_err(super::error::Error::from)?;
        Ok(Some((bytes, index)))
    }

    /// [`surface_root_status`] with this source's `allow_yanked`; tag path only.
    fn surface_status(&self, identifier: &ocx_oci::PackageRef, root: &IndexRoot, tag: &RootTag) -> Result<()> {
        surface_root_status(identifier, root, tag, self.allow_yanked)
    }

    /// Resolves a tag-addressed identifier to its verified dispatch object, surfacing its status.
    /// `Ok(None)` when the package or tag is absent.
    async fn resolve_tag(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::ImageIndex)>> {
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

    /// One live `GET` of the root for `repository`, paired with the sha256 of the served bytes.
    /// Never memoized and never committed locally; `Ok(None)` only on a confirmed 404.
    ///
    /// The root read is the canonical one only when this source was built with an empty
    /// `[mirrors]` index map; otherwise it is the mirror's copy.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat) on an unknown
    /// format version; [`Error::MalformedIndexDocument`](super::error::Error::MalformedIndexDocument)
    /// when the served root does not parse; the transport error otherwise.
    pub async fn fetch_root_uncached(&self, repository: &str) -> Result<Option<(ocx_oci::Digest, IndexRoot)>> {
        // Uncoalesced gate: `check_format_version` wraps a refusal in `SourceFetchFailed`, and callers
        // of this one-shot read match the typed `UnsupportedIndexFormat`.
        let gated = self.cache.read().await.config.is_some();
        if !gated {
            self.fetch_format_config().await?;
        }
        Ok(self.fetch_root(repository, Freshness::Revalidate).await?.map(|cached| {
            (
                ocx_oci::Algorithm::Sha256.hash(&*cached.bytes),
                Arc::unwrap_or_clone(cached.parsed),
            )
        }))
    }

    /// The physical [`ocx_oci::OciIdentifier`] `root`'s `repository` pointer names, after the
    /// SSRF guard has judged its host against this source's trusted and insecure hosts.
    ///
    /// # Errors
    ///
    /// [`Error::MalformedPhysicalRef`](super::error::Error::MalformedPhysicalRef) for an
    /// unparseable pointer; [`Error::Ssrf`](super::error::Error::Ssrf) for a forbidden target.
    pub async fn guard_repository_pointer(&self, root: &IndexRoot) -> Result<ocx_oci::OciIdentifier> {
        let physical = super::parse_repository_pointer(&root.repository)?;
        let registry = physical.registry();
        // SSRF floor: `registry` comes from remote-controlled index data, so it is validated before
        // any physical request; `self.client`'s `GuardedResolver` pins the validated address.
        let (host, port) = ocx_oci::ssrf::split_host_port(registry);
        ocx_oci::ssrf::guard_destination(
            // The scheme decides whether `HTTP_PROXY` or `HTTPS_PROXY` routes the dial.
            ocx_oci::ssrf::DialScheme::for_registry(self.insecure_hosts(), registry),
            host,
            port,
            &self.trusted_hosts,
            &self.proxy_rules,
        )
        .await
        .map_err(|source| super::error::Error::Ssrf {
            source: ocx_oci::ssrf::PhysicalDialRefused {
                namespace: self.namespace.clone(),
                source,
            },
        })?;
        Ok(physical)
    }

    /// The physical [`ocx_oci::OciIdentifier`] the root's `repository` pointer names, at
    /// `identifier`'s tag/digest; transport-only.
    async fn physical_identifier(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        let physical = self.guard_repository_pointer(&root).await?;
        Ok(Some(physical.at_version_of(identifier)))
    }

    // ── catalog ──────────────────────────────────────────────────────────────

    /// Fetches this source's live `c/index.json`; nothing is persisted from it.
    ///
    /// A 404 yields an empty catalog; a caller acting on the enumeration wants
    /// [`Self::fetch_catalog_strict`].
    pub async fn fetch_catalog(&self) -> Result<CatalogIndex> {
        Ok(self.fetch_catalog_document().await?.unwrap_or_else(CatalogIndex::new))
    }

    /// [`Self::fetch_catalog`], but an absent catalog is an error: read as empty, `index sync`
    /// would exit 0 having refreshed nothing. A served catalog with zero packages is still `Ok`.
    pub async fn fetch_catalog_strict(&self) -> Result<CatalogIndex> {
        let url = format!("{}/c/index.json", self.base_url);
        self.fetch_catalog_document()
            .await?
            .ok_or_else(|| super::error::Error::CatalogDocumentAbsent {
                index_source: self.namespace.clone(),
                url,
            })
    }

    /// The catalog document, or `None` when the source serves none.
    async fn fetch_catalog_document(&self) -> Result<Option<CatalogIndex>> {
        self.check_format_version().await?;
        let url = format!("{}/c/index.json", self.base_url);
        Ok(match self.transport.get(&url).await? {
            IndexFetch::NotFound => None,
            IndexFetch::Found { bytes } => Some(parse_document::<CatalogDocument>(&bytes, &url)?.into_packages()?),
        })
    }
}

/// Warns on yank / deprecation / supersession and refuses a yanked tag unless `allow_yanked`.
/// Shared by the live resolve and [`LocalIndex::resolve_dispatch`](super::LocalIndex) so a yank
/// is honored identically online and offline; tag path only, never on a digest pin.
pub(super) fn surface_root_status(
    identifier: &ocx_oci::PackageRef,
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
            });
        }
    }
    if root.status.as_deref() == Some("deprecated") {
        match &root.deprecated_message {
            Some(message) => log::warn!("'{identifier}' is deprecated: {message}"),
            None => log::warn!("'{identifier}' is deprecated"),
        }
    }
    // Advisory only: following the successor would override the requested identity.
    if let Some(successor) = &root.superseded_by {
        log::warn!("'{identifier}' is superseded by '{successor}' (advisory; not followed automatically)");
    }
    Ok(())
}

/// Resolves the absent-`config.json` case to [`IndexFormatConfig::assumed_v1`], outside the
/// fetch so the assumed value never reaches a memo or a coalescing group.
fn or_assumed_v1(served: Option<Arc<IndexFormatConfig>>) -> Arc<IndexFormatConfig> {
    served.unwrap_or_else(|| Arc::new(IndexFormatConfig::assumed_v1()))
}

/// Parses `bytes` as `T`, naming the source `url` on failure.
fn parse_document<T: for<'de> Deserialize<'de>>(bytes: &[u8], url: &str) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|source| super::error::Error::MalformedIndexDocument {
        url: redact_url(url),
        source,
    })
}

#[async_trait]
impl index_impl::IndexImpl for OcxIndex {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        if registry != self.namespace {
            return Ok(Vec::new());
        }
        let mut repositories: Vec<String> = self.fetch_catalog().await?.into_keys().collect();
        repositories.sort();
        repositories.dedup();
        Ok(repositories)
    }

    async fn list_tags(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
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
        identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }

        // Digest-addressed: the digest is the platform-manifest leaf, fetched physically.
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

        let Some((content, index)) = self.resolve_tag(identifier).await? else {
            return Ok(None);
        };
        Ok(Some((content, ocx_oci::Manifest::ImageIndex(index))))
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>> {
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }
        if let Some(digest) = identifier.digest() {
            return Ok(Some(digest));
        }
        Ok(self.resolve_tag(identifier).await?.map(|(digest, _)| digest))
    }

    async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
        if !self.serves_registry(blob_ref.as_identifier().registry()) {
            return Ok(None);
        }
        let Some(physical) = self.physical_identifier(blob_ref.as_identifier()).await? else {
            return Ok(None);
        };
        Ok(Some(self.client.pull_blob(&physical.at_pin_of(blob_ref)).await?))
    }

    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }

        // Leaf: the physical registry's verbatim bytes hash to the leaf digest, valid to persist.
        if identifier.digest().is_some() {
            let Some(physical) = self.physical_identifier(identifier).await? else {
                return Ok(None);
            };
            return Ok(self
                .client
                .fetch_manifest_raw_bytes_addressed(&physical, ReadAddressing::Mirrored)
                .await?);
        }

        // Tag: verbatim bytes, never a re-serialisation, so a key this client does not model survives.
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
        Ok(Some((bytes, content, ocx_oci::Manifest::ImageIndex(index))))
    }

    async fn fetch_root_document(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<(Vec<u8>, IndexRoot)>> {
        // Verbatim bytes: re-serialised, they would stop hashing to the catalog entry.
        // Checked before the memo: a foreign registry must memoize nothing (see `memoize_root`).
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }
        // The version gate runs before any root is consumed.
        self.check_format_version().await?;
        // A memoized root is the whole answer, hit and miss alike.
        if let Some(cached) = self.cache.read().await.roots.get(identifier.repository()) {
            return Ok(cached
                .as_ref()
                .map(|cached| ((*cached.bytes).clone(), (*cached.parsed).clone())));
        }
        let url = format!("{}/p/{}.json", self.base_url, identifier.repository());
        // Both arms issued the request, so both memoize; otherwise the per-tag fan-out that
        // follows re-fetches this root once per tag, which coalescing alone would not prevent.
        match self.transport.get(&url).await? {
            IndexFetch::Found { bytes } => {
                let root: IndexRoot = parse_document(&bytes, &url)?;
                self.memoize_root(
                    identifier.repository(),
                    Some(CachedRoot {
                        bytes: Arc::new(bytes.clone()),
                        parsed: Arc::new(root.clone()),
                    }),
                )
                .await;
                Ok(Some((bytes, root)))
            }
            IndexFetch::NotFound => {
                self.memoize_root(identifier.repository(), None).await;
                Ok(None)
            }
        }
    }

    async fn revalidate_root_document(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<(Vec<u8>, IndexRoot)>> {
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }
        self.check_format_version().await?;
        let fetched = self.fetch_root(identifier.repository(), Freshness::Revalidate).await?;
        // Replaces the memo, so the dispatch reads that follow agree with the root just committed.
        self.memoize_root(identifier.repository(), fetched.clone()).await;
        Ok(fetched.map(|cached| ((*cached.bytes).clone(), (*cached.parsed).clone())))
    }

    async fn physical_reference(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }
        self.physical_identifier(identifier).await
    }

    fn jurisdiction(&self, identifier: &ocx_oci::PackageRef) -> super::Jurisdiction {
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
    use ocx_oci::Algorithm;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

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
        /// URLs asked for through [`IndexTransport::get_uncached`].
        uncached: StubRequests,
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

        fn uncached_urls(&self) -> Vec<String> {
            self.uncached.lock().unwrap().clone()
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
                });
            }
            let responses = self.responses.lock().unwrap();
            match responses.get(url) {
                Some(bytes) => Ok(IndexFetch::Found { bytes: bytes.clone() }),
                None => Ok(IndexFetch::NotFound),
            }
        }

        async fn get_uncached(&self, url: &str) -> Result<IndexFetch> {
            self.uncached.lock().unwrap().push(url.to_string());
            self.get(url).await
        }

        fn box_clone(&self) -> Box<dyn IndexTransport> {
            Box::new(self.clone())
        }
    }

    // ── construction helpers ─────────────────────────────────────────────────

    fn stub_client() -> ocx_oci::Client {
        // The physical OCI client is never reached on the tag-resolution paths
        // these tests exercise; an empty stub satisfies construction.
        ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())))
    }

    fn make_source(transport: StubIndexTransport, allow_yanked: bool) -> OcxIndex {
        make_source_with(transport, allow_yanked, stub_client(), Vec::new())
    }

    /// Like [`make_source`] but with an explicit physical-fetch client and
    /// `trusted_hosts` — used by the SSRF read-path tests.
    fn make_source_with(
        transport: StubIndexTransport,
        allow_yanked: bool,
        client: ocx_oci::Client,
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
            proxy_rules: ocx_oci::ssrf::ProxyRules::direct(),
        })
    }

    fn config_url() -> String {
        format!("{BASE}/config.json")
    }
    fn root_url() -> String {
        format!("{BASE}/p/{REPO}.json")
    }
    fn dispatch_url(digest: &ocx_oci::Digest) -> String {
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
    fn tagged_id() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(REPO, NAMESPACE).clone_with_tag("3.28")
    }

    /// A two-platform OCI image index (glibc + musl leaves) as the verbatim
    /// bytes a registry served.
    ///
    /// **Deliberately NOT the canonical serde encoding of what it parses to.**
    /// It is pretty-printed and carries a `subject` field `ocx_oci::ImageIndex` does
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
    fn seed_package(transport: &StubIndexTransport, yanked: bool) -> ocx_oci::Digest {
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
        assert_eq!(tag.content, ocx_oci::Digest::Sha256("a".repeat(64)));
        assert_eq!(
            tag.yanked,
            Some(YankMarker {
                reason: "critical security issue".to_string(),
                at: "2026-02-01T00:00:00Z".to_string(),
            })
        );

        let dispatch: ocx_oci::ImageIndex = serde_json::from_slice(glibc_musl_index()).unwrap();
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

        let glibc_host: ocx_oci::Platform = "linux/amd64+libc.glibc".parse().unwrap();
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
            ocx_oci::Manifest::ImageIndex(index) => assert_eq!(index.manifests.len(), 2),
            other => panic!("expected a parsed image index, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_manifest_surfaces_malformed_index_document_for_bad_tag_content_digest() {
        // `RootTag::content`'s `ocx_oci::Digest` deserialize is exact-wire
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
                super::super::error::Error::MalformedIndexDocument { .. }
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
    fn digest_id() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(REPO, NAMESPACE).clone_with_digest(ocx_oci::Digest::Sha256("b".repeat(64)))
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
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(recorder.clone())));
        let source = make_source_with(transport, false, client, Vec::new());

        let error = source
            .fetch_manifest(&digest_id(), IndexOperation::Resolve)
            .await
            .expect_err("a forbidden physical host must be refused");
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
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let refused = make_source_with(transport, false, client, Vec::new());
        assert!(
            matches!(
                refused.fetch_manifest(&digest_id(), IndexOperation::Resolve).await,
                Err(super::super::error::Error::Ssrf { .. })
            ),
            "the metadata endpoint must be refused by default"
        );

        let transport = StubIndexTransport::new();
        seed_root_pointing_at(&transport, "oci://169.254.169.254/x");
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let trusted = make_source_with(transport, false, client, vec!["169.254.169.254".to_string()]);
        let error = trusted
            .fetch_manifest(&digest_id(), IndexOperation::Resolve)
            .await
            .expect_err("no manifest is seeded, so the physical fetch itself fails");
        assert!(
            !matches!(error, super::super::error::Error::Ssrf { .. }),
            "a trusted host must pass the SSRF guard (failure must come from the fetch, not the guard); got {error:?}"
        );
    }

    // ── canonical root read ──────────────────────────────────────────────────

    /// Deliberately not the canonical serialisation of what it parses to: a digest taken over a
    /// re-serialised root, rather than over the served bytes, cannot equal the digest of this text.
    const SERVED_ROOT: &str = concat!(
        "{\n",
        "   \"repository\" :  \"oci://ghcr.io/ocx-contrib/cmake\",\n",
        "   \"tags\" : { \"3.28\" : { \"content\" : \"sha256:",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "\" } }\n",
        "}\n"
    );

    #[tokio::test]
    async fn fetch_root_uncached_returns_the_sha256_of_the_served_bytes_and_the_parsed_root() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert(&root_url(), SERVED_ROOT.as_bytes());
        let source = make_source(transport, false);

        let (digest, root) = source
            .fetch_root_uncached(REPO)
            .await
            .expect("a served root reads")
            .expect("a served root is present");

        assert_eq!(digest, Algorithm::Sha256.hash(SERVED_ROOT.as_bytes()));
        assert_eq!(root.repository, "oci://ghcr.io/ocx-contrib/cmake");
        assert!(root.tags.contains_key("3.28"));
    }

    #[tokio::test]
    async fn removal_deciding_root_reads_bypass_http_caches() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let source = make_source(transport.clone(), false);

        source
            .fetch_root_uncached(REPO)
            .await
            .expect("prune's read")
            .expect("root present");
        source
            .revalidate_root_document(&tagged_id())
            .await
            .expect("the update read")
            .expect("root present");

        assert_eq!(
            transport.uncached_urls(),
            [root_url(), root_url()],
            "both removal-deciding root reads must tell HTTP caches to revalidate"
        );
    }

    #[tokio::test]
    async fn ordinary_reads_leave_http_caches_alone() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let source = make_source(transport.clone(), false);

        source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect("resolve")
            .expect("the tag resolves");
        source.fetch_root_document(&tagged_id()).await.expect("grow read");

        assert!(
            transport.request_count(&root_url()) > 0,
            "non-vacuity: the ordinary reads reached the root"
        );
        assert!(
            transport.uncached_urls().is_empty(),
            "a resolve must stay cacheable: {:?}",
            transport.uncached_urls()
        );
    }

    #[tokio::test]
    async fn revalidate_root_document_bypasses_and_replaces_the_memo() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let source = make_source(transport.clone(), false);
        source
            .resolve_root(REPO)
            .await
            .expect("memoizing read")
            .expect("root present");

        transport.insert(&root_url(), SERVED_ROOT.as_bytes());
        let (bytes, _) = source
            .revalidate_root_document(&tagged_id())
            .await
            .expect("revalidated read")
            .expect("root present");

        assert_eq!(
            bytes,
            SERVED_ROOT.as_bytes(),
            "a warm memo must not answer a revalidated read"
        );
        let memoized = source
            .resolve_root(REPO)
            .await
            .expect("memo read")
            .expect("root present");
        assert_eq!(
            memoized.tags.get("3.28").map(|tag| tag.content.to_string()),
            Some(format!("sha256:{}", "a".repeat(64))),
            "the later reads see the revalidated root"
        );
        assert_eq!(
            transport.request_count(&root_url()),
            2,
            "the read after revalidation is answered from the replaced memo"
        );
    }

    #[tokio::test]
    async fn fetch_root_uncached_is_none_when_the_root_is_not_served() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport, false);

        let root = source
            .fetch_root_uncached(REPO)
            .await
            .expect("a 404 root is an answer, not an error");

        assert!(root.is_none());
    }

    #[tokio::test]
    async fn fetch_root_uncached_asks_the_wire_on_every_call() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert(&root_url(), SERVED_ROOT.as_bytes());
        let source = make_source(transport.clone(), false);

        source.fetch_root_uncached(REPO).await.expect("first read");
        source.fetch_root_uncached(REPO).await.expect("second read");

        assert_eq!(
            transport.request_count(&root_url()),
            2,
            "the uncached read is never memoized: it must observe the wire as it is now"
        );
    }

    #[tokio::test]
    async fn fetch_root_uncached_bypasses_a_root_the_source_already_memoized() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let source = make_source(transport.clone(), false);
        source
            .resolve_root(REPO)
            .await
            .expect("the memoizing read")
            .expect("the root is served");
        assert_eq!(transport.request_count(&root_url()), 1);

        source
            .fetch_root_uncached(REPO)
            .await
            .expect("uncached read")
            .expect("root present");

        assert_eq!(
            transport.request_count(&root_url()),
            2,
            "a warm memo must not answer an uncached read"
        );
    }

    #[tokio::test]
    async fn fetch_root_uncached_does_not_seed_the_memo_the_ordinary_read_uses() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert(&root_url(), SERVED_ROOT.as_bytes());
        let source = make_source(transport.clone(), false);
        source.fetch_root_uncached(REPO).await.expect("uncached read");
        assert_eq!(transport.request_count(&root_url()), 1);

        source
            .resolve_root(REPO)
            .await
            .expect("the ordinary read")
            .expect("the root is served");

        assert_eq!(
            transport.request_count(&root_url()),
            2,
            "the ordinary read must not be answered from an uncached read's result"
        );
    }

    #[tokio::test]
    async fn fetch_root_uncached_surfaces_a_transport_failure_as_an_error() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.fail(&root_url());
        let source = make_source(transport, false);

        let error = source
            .fetch_root_uncached(REPO)
            .await
            .expect_err("a dead endpoint is not an absent root");

        assert!(
            matches!(error, super::super::error::Error::IndexHttpFailed { .. }),
            "expected the transport error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_root_uncached_refuses_an_unsupported_format_version_before_reading_the_root() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":99}"#);
        transport.insert(&root_url(), SERVED_ROOT.as_bytes());
        let source = make_source(transport.clone(), false);

        let error = source
            .fetch_root_uncached(REPO)
            .await
            .expect_err("an unknown format version is refused");

        assert!(
            matches!(error, super::super::error::Error::UnsupportedIndexFormat { .. }),
            "expected the format-version refusal, got {error:?}"
        );
        assert_eq!(transport.request_count(&root_url()), 0);
    }

    // ── repository-pointer guard ─────────────────────────────────────────────

    fn root_pointing_at(pointer: &str) -> IndexRoot {
        serde_json::from_str(&format!(r#"{{"repository":"{pointer}","tags":{{}}}}"#)).expect("a root parses")
    }

    #[tokio::test]
    async fn guard_repository_pointer_refuses_a_forbidden_host() {
        let source = make_source(StubIndexTransport::new(), false);

        let error = source
            .guard_repository_pointer(&root_pointing_at("oci://127.0.0.1/x"))
            .await
            .expect_err("a loopback pointer must be refused");

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
            "expected an SSRF ForbiddenTarget refusal, got {error:?}"
        );
    }

    #[tokio::test]
    async fn guard_repository_pointer_admits_a_forbidden_host_the_operator_trusts() {
        let source = make_source_with(
            StubIndexTransport::new(),
            false,
            stub_client(),
            vec!["169.254.169.254".to_string()],
        );

        let physical = source
            .guard_repository_pointer(&root_pointing_at("oci://169.254.169.254/x"))
            .await
            .expect("a trusted host passes the guard");

        assert_eq!(physical.registry(), "169.254.169.254");
    }

    #[tokio::test]
    async fn guard_repository_pointer_returns_the_parsed_identifier_for_a_permitted_host() {
        let source = make_source(StubIndexTransport::new(), false);

        let physical = source
            .guard_repository_pointer(&root_pointing_at("oci://93.184.216.34/ocx-contrib/cmake"))
            .await
            .expect("a public address passes the guard");

        assert_eq!(physical.registry(), "93.184.216.34");
        assert_eq!(physical.repository(), "ocx-contrib/cmake");
    }

    #[tokio::test]
    async fn guard_repository_pointer_refuses_an_unparseable_pointer() {
        let source = make_source(StubIndexTransport::new(), false);

        let error = source
            .guard_repository_pointer(&root_pointing_at("not a pointer"))
            .await
            .expect_err("an unparseable pointer is refused");

        assert!(
            matches!(error, super::super::error::Error::MalformedPhysicalRef { .. }),
            "expected MalformedPhysicalRef, got {error:?}"
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
            serde_json::from_slice::<ocx_oci::ImageIndex>(substituted).is_ok(),
            "the substituted payload must parse, or the digest check is not what refuses it"
        );
        transport.insert(&dispatch_url(&honest_digest), substituted);

        let source = make_source(transport, false);
        let error = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("substituted dispatch-object bytes must not load");
        assert!(
            matches!(error, super::super::error::Error::DispatchObjectDigestMismatch { .. }),
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
            matches!(error, super::super::error::Error::MalformedIndexDocument { .. }),
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
    /// produced by serialising an `ocx_oci::ImageIndex`, so a fixture built by the
    /// code under test could never contradict it.
    #[tokio::test]
    async fn resolve_index_object_refuses_a_wrong_schema_version() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let body = br#"{"schemaVersion":1,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        assert!(
            serde_json::from_slice::<ocx_oci::ImageIndex>(body).is_ok(),
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
            matches!(error, super::super::error::Error::InvalidImageIndex(_)),
            "expected InvalidImageIndex, got {error:?}"
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
                super::super::error::Error::UnsupportedIndexFormat { version: 2 }
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
            matches!(error, super::super::error::Error::YankedRefused { .. }),
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
        let pinned = ocx_oci::Digest::Sha256("c".repeat(64));
        let id = ocx_oci::PackageRef::new_registry(REPO, NAMESPACE).clone_with_digest(pinned.clone());

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

    fn no_mirrors() -> BTreeMap<String, ocx_oci::client::mirror_map::ParsedMirror> {
        BTreeMap::new()
    }

    #[test]
    fn resolve_base_url_defaults_and_honors_registries_index() {
        let empty = ocx_config::Config::default();
        assert_eq!(
            OcxIndex::resolve_base_url(
                &empty,
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default()
            )
            .unwrap()
            .url,
            ocx_config::index::DEFAULT_INDEX_BASE_URL,
            "no [registries.\"ocx.sh\"] index field must yield the default base URL"
        );

        let config: ocx_config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://artifactory.corp/ocx-index/\"").unwrap();
        assert_eq!(
            OcxIndex::resolve_base_url(
                &config,
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default()
            )
            .unwrap()
            .url,
            "https://artifactory.corp/ocx-index",
            "[registries.\"<ns>\"] index must replace the base URL (trailing slash trimmed)"
        );
        assert_eq!(
            OcxIndex::resolve_base_url(
                &config,
                "other.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default()
            )
            .unwrap()
            .url,
            ocx_config::index::DEFAULT_INDEX_BASE_URL,
            "an unlisted namespace falls back to the default"
        );
    }

    #[test]
    fn resolve_base_url_gates_plain_http_target() {
        let config: ocx_config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"http://mirror.corp/ocx-index\"").unwrap();

        // http base without the host in the insecure list → hard config error.
        let error = OcxIndex::resolve_base_url(
            &config,
            "ocx.sh",
            &no_mirrors(),
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
        .expect_err("an http index base must be refused without an insecure-host listing");
        assert!(
            matches!(
                error,
                super::super::error::Error::PlainHttpIndexNotAllowed { ref host, .. }
                    if host == "mirror.corp"
            ),
            "expected PlainHttpIndexNotAllowed naming the host, got {error:?}"
        );

        // Same base allowed once the host is listed.
        let insecure = vec!["mirror.corp".to_string()];
        assert_eq!(
            OcxIndex::resolve_base_url(
                &config,
                "ocx.sh",
                &no_mirrors(),
                &insecure,
                &ocx_util::tls::ExtraRoots::default()
            )
            .unwrap()
            .url,
            "http://mirror.corp/ocx-index",
            "an http base is allowed when its host is in the resolved plain-HTTP set"
        );

        // The default https base URL is never gated.
        assert_eq!(
            OcxIndex::resolve_base_url(
                &ocx_config::Config::default(),
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default()
            )
            .unwrap()
            .url,
            ocx_config::index::DEFAULT_INDEX_BASE_URL,
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
        let config: ocx_config::Config = toml::from_str(
            "[registries.\"ocx.sh\"]\nindex = \"http://index.corp:8080/ocx-index\"\n\
             [registries.\"index.corp:8080\"]\ninsecure = true\n",
        )
        .unwrap();

        let licensed = ocx_config::insecure::insecure_hosts(&config, &[]);
        assert_eq!(
            licensed,
            vec!["index.corp:8080".to_string()],
            "precondition: the allowance comes from the config, not the environment"
        );

        assert_eq!(
            OcxIndex::resolve_base_url(
                &config,
                "ocx.sh",
                &no_mirrors(),
                &licensed,
                &ocx_util::tls::ExtraRoots::default()
            )
            .expect("a config-licensed plain-HTTP index base must resolve")
            .url,
            "http://index.corp:8080/ocx-index",
        );
        assert!(
            OcxIndex::resolve_base_url(
                &config,
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default()
            )
            .is_err(),
            "and it must still be refused when nothing licenses it"
        );
    }

    #[test]
    fn resolve_base_url_gates_mixed_case_http_scheme() {
        // CWE-319 regression: a mixed-case `HTTP://` index base must hit the
        // plain-HTTP gate exactly like lowercase `http://` — the scheme is
        // normalized to lowercase in `parse_url`, so the gate's `== "http"`
        // comparison cannot be bypassed by casing.
        let config: ocx_config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"HTTP://mirror.corp/ocx-index\"").unwrap();

        let error = OcxIndex::resolve_base_url(
            &config,
            "ocx.sh",
            &no_mirrors(),
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
        .expect_err("a mixed-case HTTP:// index base must be refused without an insecure-host listing");
        assert!(
            matches!(
                error,
                super::super::error::Error::PlainHttpIndexNotAllowed { ref host, .. }
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
            ocx_config::mirror::parse_url("https://artifactory.corp/ocx-index").unwrap(),
        );

        assert_eq!(
            OcxIndex::resolve_base_url(
                &ocx_config::Config::default(),
                "ocx.sh",
                &mirrors_index,
                &[],
                &ocx_util::tls::ExtraRoots::default()
            )
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
            ocx_config::mirror::parse_url("https://artifactory.corp/ocx-index").unwrap(),
        );
        assert_eq!(
            OcxIndex::resolve_base_url(
                &ocx_config::Config::default(),
                "ocx.sh",
                &unrelated_mirror,
                &[],
                &ocx_util::tls::ExtraRoots::default()
            )
            .unwrap()
            .url,
            ocx_config::index::DEFAULT_INDEX_BASE_URL,
            "a mirror keyed by an unrelated host must not affect this base URL"
        );
    }

    // ── closed scheme set (C-018/C-019/C-020) ────────────────────────────────

    fn index_config(url: &str) -> ocx_config::Config {
        toml::from_str(&format!("[registries.\"ocx.sh\"]\nindex = \"{url}\"")).unwrap()
    }

    /// The `InvalidIndexUrl` a refused scheme must raise, with its exit code.
    fn expect_invalid_index_url(error: &crate::error::Error, expected_origin: &str) {
        let super::super::error::Error::InvalidIndexUrl { origin, .. } = error else {
            panic!("expected InvalidIndexUrl, got {error:?}");
        };
        assert_eq!(*origin, expected_origin, "the diagnostic must name the setting to fix");
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
            &ocx_util::tls::ExtraRoots::default(),
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
            let error = OcxIndex::resolve_base_url(
                &index_config(base),
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default(),
            )
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
            let resolved = OcxIndex::resolve_base_url(
                &index_config(base),
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default(),
            )
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
        let base = OcxIndex::resolve_base_url(
            &index_config("file:///srv/ocx-index"),
            "ocx.sh",
            &no_mirrors(),
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
        .expect("a file:// base with an empty authority and an absolute path is permitted");
        assert_eq!(base.url, "file:///srv/ocx-index");

        let trimmed = OcxIndex::resolve_base_url(
            &index_config("file:///srv/ocx-index/"),
            "ocx.sh",
            &no_mirrors(),
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
        .expect("a trailing slash is trimmed, matching every other base");
        assert_eq!(trimmed.url, "file:///srv/ocx-index");

        // A drive with a directory under it stays valid — the bare-drive
        // refusal must not swallow the Windows form C-018's table admits.
        let drive = OcxIndex::resolve_base_url(
            &index_config("file:///C:/srv/x"),
            "ocx.sh",
            &no_mirrors(),
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
        .expect("a drive-qualified path is a valid file base");
        assert_eq!(drive.url, "file:///C:/srv/x");
    }

    #[test]
    fn resolve_base_url_refuses_a_single_slash_file_base() {
        // C-003/S-002 (#382): `file:/srv/x` holds no `://`, so before check 1a
        // it read as schemeless, defaulted to https, and `parse_url` split it
        // into host `file:` + path `srv/x` — an `IndexBase` pointed at a host
        // named `file`, failing much later as a DNS lookup.
        let error = OcxIndex::resolve_base_url(
            &index_config("file:/srv/x"),
            "ocx.sh",
            &no_mirrors(),
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
        .expect_err("a file base written with one slash must be refused");
        let rendered = error.to_string();
        assert!(
            rendered.contains("file:///srv/x"),
            "the refusal must name the corrected spelling: {rendered}"
        );

        // `FILE:` is the same mistake shouted, and `file:` alone names no
        // directory at all. Neither may reach the https default.
        for base in ["FILE:/srv/x", "file:", "file:srv/x"] {
            let error = OcxIndex::resolve_base_url(
                &index_config(base),
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default(),
            )
            .expect_err("a single-colon file base must be refused whatever follows it");
            assert!(
                error.to_string().contains("file://"),
                "the refusal must name the spelling that works: {error}"
            );
        }

        // A relative tail gets the shape, not a literal: `file://srv/x` names a
        // non-empty authority, which `resolve_file_base` refuses in turn.
        let error = OcxIndex::resolve_base_url(
            &index_config("file:srv/x"),
            "ocx.sh",
            &no_mirrors(),
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
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
            let resolved = OcxIndex::resolve_base_url(
                &index_config(base),
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default(),
            )
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
            let error = OcxIndex::resolve_base_url(
                &index_config(base),
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default(),
            )
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
            let error = OcxIndex::resolve_base_url(
                &index_config(base),
                "ocx.sh",
                &no_mirrors(),
                &[],
                &ocx_util::tls::ExtraRoots::default(),
            )
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
            ocx_config::mirror::parse_url("file://localhost/srv/x").unwrap(),
        );

        let error = OcxIndex::resolve_base_url(
            &index_config("https://up.example"),
            "ocx.sh",
            &mirrors_index,
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
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
            ocx_config::mirror::parse_url("https://artifactory.corp").unwrap(),
        );

        let base = OcxIndex::resolve_base_url(
            &index_config("file:///srv/x"),
            "ocx.sh",
            &mirrors_index,
            &[],
            &ocx_util::tls::ExtraRoots::default(),
        )
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

        let foreign = ocx_oci::PackageRef::new_registry("cmake", "ghcr.io").clone_with_tag("3.28");
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
    fn seed_empty_index(transport: &StubIndexTransport, repo: &str, tag: &str) -> ocx_oci::Digest {
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

    fn registry_manifest() -> (Vec<u8>, ocx_oci::Digest) {
        let manifest = ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default());
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(id)
                .await?
                .map(|(_, digest, m)| (digest, m)))
        }
        async fn fetch_manifest_digest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
            *self.calls.lock().unwrap() += 1;
            let (bytes, digest) = registry_manifest();
            Ok(Some((
                bytes,
                digest,
                ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
            )))
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn local_index(dir: &tempfile::TempDir) -> super::super::LocalIndex {
        super::super::LocalIndex::new(super::super::LocalConfig {
            index_store: crate::IndexStore::new(dir.path().join("index")),
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

        let id = ocx_oci::PackageRef::new_registry("ns/pkg", NAMESPACE).clone_with_tag("1.0");
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
        // points at, with the logical tag and leaf digest carried over
        // (transport-only, C2).
        let leaf = ocx_oci::Digest::Sha256("a".repeat(64));
        let logical = ocx_oci::PackageRef::new_registry(REPO, NAMESPACE)
            .clone_with_tag("3.28")
            .clone_with_digest(leaf.clone());
        let physical = source
            .physical_reference(&logical)
            .await
            .unwrap()
            .expect("an own-namespace reference maps to a physical location");
        assert_eq!(physical.registry(), "ghcr.io");
        assert_eq!(physical.repository(), "ocx-contrib/cmake");
        assert_eq!(physical.digest(), Some(leaf));
        assert_eq!(physical.tag(), Some("3.28"), "the logical tag survives the rewrite");
        assert_eq!(
            source.jurisdiction(&logical),
            super::super::Jurisdiction::Authoritative,
            "the source owns its namespace"
        );

        // A foreign namespace is neither rewritten nor owned.
        let foreign = ocx_oci::PackageRef::new_registry("x/y", "ghcr.io")
            .clone_with_digest(ocx_oci::Digest::Sha256("b".repeat(64)));
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
        let foreign = ocx_oci::PackageRef::new_registry("x/y", "ghcr.io").clone_with_tag("1.0");
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

        let first = ocx_oci::PackageRef::new_registry("a/one", NAMESPACE).clone_with_tag("1.0");
        let second = ocx_oci::PackageRef::new_registry("b/two", NAMESPACE).clone_with_tag("1.0");
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

        let second_id = ocx_oci::PackageRef::new_registry("other/pkg", NAMESPACE).clone_with_tag("1.0");
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

    /// ocx#424 / V-11 — a root already resolved is **not** re-fetched when its
    /// verbatim bytes are asked for.
    ///
    /// This pair is the shipped sequence of a first-sight, digest-addressed
    /// resolve: `ChainedIndex::physical_reference` dereferences the root
    /// through `resolve_root`, and `record_routing_pointer` then asks
    /// `fetch_root_document` for the same document's bytes so it can commit a
    /// local routing pointer. Two calls, one document, one GET.
    ///
    /// *Red-reachability:* memoize the parse alone — `roots` as
    /// `Option<Arc<IndexRoot>>`, which is what shipped — and the count is 2,
    /// because a memoized hit cannot answer for bytes it does not hold. The
    /// assertion is on the REQUEST COUNT and on the bytes: an answer that
    /// re-serialised the parse would keep the count at 1 and break F1.
    #[tokio::test]
    async fn a_resolved_root_is_not_re_fetched_for_its_verbatim_bytes() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        // Non-canonical whitespace, for the reason the sibling above states.
        let root_bytes = br#"{  "repository" : "oci://ghcr.io/ocx-contrib/cmake" ,  "tags" : { }  }"#.to_vec();
        transport.insert(&root_url(), &root_bytes);
        let source = make_source(transport.clone(), false);

        source
            .resolve_root(REPO)
            .await
            .unwrap()
            .expect("the published root is served");
        let (bytes, _) = source
            .fetch_root_document(&tagged_id())
            .await
            .unwrap()
            .expect("the root already in the memo still answers for its bytes");

        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "ocx#424 — the routing-pointer write must reuse the root the resolve just fetched"
        );
        assert_eq!(
            bytes, root_bytes,
            "and the memo must hand back the verbatim bytes, never a re-serialisation (F1)"
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
        let foreign = ocx_oci::PackageRef::new_registry(REPO, "other.io").clone_with_tag("3.28");
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
            .fetch_root_document(&ocx_oci::PackageRef::new_registry(REPO, NAMESPACE))
            .await
            .unwrap()
            .expect("the published root is served");
        for tag in ["3.28", "3.29"] {
            source
                .fetch_manifest(
                    &ocx_oci::PackageRef::new_registry(REPO, NAMESPACE).clone_with_tag(tag),
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
        let identifier = ocx_oci::PackageRef::new_registry(REPO, NAMESPACE);

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

        let foreign = ocx_oci::PackageRef::new_registry(REPO, "other.io");
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

    fn flat_id() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(FLAT_REPO, NAMESPACE).clone_with_tag("3")
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

        let foreign = ocx_oci::PackageRef::new_registry(REPO, "ghcr.io").clone_with_tag("3.28");
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
    fn assert_names_the_index(error: &crate::error::Error, identifier: &ocx_oci::PackageRef) {
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

        let absent = ocx_oci::PackageRef::new_registry("ns/absent", NAMESPACE).clone_with_tag("1.0");
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
        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(FLAT_REPO, NAMESPACE)
                .clone_with_digest(ocx_oci::Digest::Sha256("c".repeat(64))),
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

/// The retry ladder and the timeout bounds against a real socket through the
/// production [`ReqwestIndexTransport`].
///
/// These are the half the virtual-clock tests in [`ocx_oci::transport_policy`]
/// cannot cover: that `get` *reads* the header, *classifies* the status and
/// *composes* the three timeout bounds. The clock is real on purpose — a
/// paused clock auto-advances whenever the runtime is idle waiting on a
/// socket, which fires the very timeouts [`TransportHardening`] exists to
/// bound. The bounds are injected instead, so the same semantics cost
/// milliseconds rather than the shipped minutes.
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
        /// Every request's header lines, lowercased, in arrival order.
        headers: RequestHeaders,
    }

    type RequestHeaders = Arc<Mutex<Vec<Vec<String>>>>;

    impl StubIndexEndpoint {
        async fn start(script: Vec<Reply>) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let served = Arc::new(AtomicUsize::new(0));

            let counter = Arc::clone(&served);
            let targets = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&targets);
            let headers: RequestHeaders = Arc::new(Mutex::new(Vec::new()));
            let header_log = Arc::clone(&headers);
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let script = script.clone();
                    let counter = Arc::clone(&counter);
                    let log = Arc::clone(&log);
                    let header_log = Arc::clone(&header_log);
                    tokio::spawn(async move {
                        let index = counter.fetch_add(1, Ordering::SeqCst);
                        let reply = script[index.min(script.len() - 1)].clone();
                        serve(socket, reply, log, header_log).await;
                    });
                }
            });

            Self {
                address,
                served,
                targets,
                headers,
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

        fn request_headers(&self) -> Vec<Vec<String>> {
            self.headers.lock().unwrap().clone()
        }
    }

    async fn serve(
        socket: tokio::net::TcpStream,
        reply: Reply,
        targets: Arc<Mutex<Vec<String>>>,
        header_log: RequestHeaders,
    ) {
        let (read_half, mut write_half) = socket.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);

        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
            return;
        }
        if let Some(target) = request_line.split_whitespace().nth(1) {
            targets.lock().unwrap().push(target.to_string());
        }
        let mut lines = Vec::new();
        loop {
            let mut header = String::new();
            match reader.read_line(&mut header).await {
                Ok(0) | Err(_) => return,
                Ok(_) if header.trim_end().is_empty() => break,
                Ok(_) => lines.push(header.trim_end().to_ascii_lowercase()),
            }
        }
        header_log.lock().unwrap().push(lines);

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

    #[tokio::test]
    async fn an_uncached_get_tells_every_http_cache_to_revalidate_and_a_plain_get_does_not() {
        let endpoint = StubIndexEndpoint::start(vec![ok()]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        transport.get_uncached(&endpoint.url()).await.expect("uncached get");
        transport.get(&endpoint.url()).await.expect("plain get");

        let headers = endpoint.request_headers();
        assert_eq!(headers.len(), 2, "one request each: {headers:?}");
        for line in ["cache-control: no-cache", "pragma: no-cache"] {
            assert!(
                headers[0].iter().any(|sent| sent == line),
                "the uncached get must send `{line}`: {:?}",
                headers[0]
            );
        }
        assert!(
            !headers[1]
                .iter()
                .any(|sent| sent.starts_with("cache-control:") || sent.starts_with("pragma:")),
            "a plain get must leave caches alone: {:?}",
            headers[1]
        );
    }

    /// Bounds generous enough that only the retry ladder is under test.
    fn quick_bounds() -> TransportHardening {
        TransportHardening {
            connect_timeout: Duration::from_secs(5),
            idle_bound: Duration::from_secs(5),
            outer_cap: Duration::from_secs(10),
        }
    }

    /// D-011: the bounds `ReqwestIndexTransport::new` ships are 30 s / 30 s /
    /// 300 s, and the `__testing` seam does not move them when it is unset.
    ///
    /// The reason this row exists at all: every other test in this module builds
    /// its transport through `with_hardening(&quick_bounds(), …)`, so **nothing
    /// asserted the shipped values** — a seam introduced over that gap could
    /// silently become the production path and no test would notice. Asserting
    /// the constructor rather than only the three constants is what binds the
    /// seam's default arm to them.
    ///
    /// The literals are spelled out, never read off the constants: reading them
    /// would make the mutation invisible, because the expectation would move
    /// with it.
    ///
    /// Mutation: change `INDEX_OUTER_CAP` to 200 s — this reds.
    #[test]
    fn the_shipped_index_bounds_are_thirty_thirty_and_three_hundred_seconds() {
        let shipped = ReqwestIndexTransport::new().hardening;
        assert_eq!(shipped.connect_timeout, Duration::from_secs(30), "connect bound");
        assert_eq!(shipped.idle_bound, Duration::from_secs(30), "idle bound");
        assert_eq!(shipped.outer_cap, Duration::from_secs(300), "per-attempt outer cap");
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

    // ── ocx#448 C-007 / DX-4 — extra CA roots reach the index client ─────────

    /// C-007 / DX-4 / S-002 (unit tier): `with_extra_roots` before the first
    /// request makes the lazily built client trust the operator's root — the
    /// seeded transport completes the handshake against an in-process server
    /// signed by a minted root (200); a transport without the call ends in
    /// `UnknownIssuer`. Dialed by IP: no resolver, no proxy in the path.
    ///
    /// Goes through `client()` rather than `get()` so the negative half reads
    /// the raw reqwest chain instead of a retry ladder's wrapped verdict.
    #[tokio::test]
    async fn extra_ca_index_transport_trusts_a_root_set_before_the_first_request() {
        use ocx_test_support::pki::{TestPki, assert_untrusted_root, error_chain, serve_https};

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let url = format!("https://{addr}/index.json");

        let seeded = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder()).with_extra_roots(
            ocx_util::tls::ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root parses"),
        );
        let response = seeded
            .client()
            .get(&url)
            .send()
            .await
            .expect("the seeded index client trusts the minted root");
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let unseeded = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());
        let error = unseeded
            .client()
            .get(&url)
            .send()
            .await
            .expect_err("without the root the index client must refuse the handshake");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);

        // Through the ladder: terminal, and the chain names the remedy the
        // raw verdict does not (review H3).
        let error = unseeded
            .get(&url)
            .await
            .expect_err("the ladder must surface the refusal, not retry it away");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);
        assert!(
            chain.contains("extra_ca_certs") && chain.contains("OCX_EXTRA_CA_CERTS"),
            "the index refusal must name the remedy: {chain}"
        );
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

    /// The status rides the variant structurally, where both the retry ladder
    /// and the exit classifier read it.
    #[tokio::test]
    async fn a_failing_status_lands_on_the_variant() {
        let endpoint = StubIndexEndpoint::start(vec![status(503)]).await;
        let transport = ReqwestIndexTransport::with_hardening(
            &quick_bounds(),
            RetryPolicy {
                attempts: 1,
                ..quick_ladder()
            },
        );

        let error = transport.get(&endpoint.url()).await.expect_err("503 is a failure");
        // The tier raises its own error now, so there is no wrapper to peel and
        // nothing for a `let ... else` to refute.
        let index_error = &error;
        match index_error {
            super::super::error::Error::IndexHttpFailed { status, .. } => assert_eq!(
                *status,
                Some(503),
                "the retry classifier reads this field; a formatted message is unreadable to it"
            ),
            other => panic!("expected IndexHttpFailed, got {other:?}"),
        }
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
        // The absolute values buy nothing here — the claim is a *ratio*: the
        // transfer outlasts a hard deadline of the idle bound's size while no
        // single frame gap approaches that bound. `BODY` is 19 bytes, so the
        // whole transfer is 19 × `interval` = 380 ms against a 110 ms bound,
        // clearing the `× 3` non-vacuity floor by 50 ms while leaving a frame
        // 90 ms of scheduling slack before it trips the bound.
        //
        // The two margins trade directly against each other — `elapsed >
        // idle_bound * 3` wants the bound low, the slack wants it high — so
        // the pair below is near the floor for this body at this `× 3`, and
        // buying more of either means a longer test, not a free win.
        let idle_bound = Duration::from_millis(110);
        let endpoint = StubIndexEndpoint::start(vec![Reply::Dribble {
            interval: Duration::from_millis(20),
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
    /// The peer dribbles one byte per interval, well inside `idle_bound`: it
    /// never trips the per-frame bound and never approaches the 32 MiB byte
    /// cap, so nothing but the outer cap can end it. C-021's two halves pass identically with or
    /// without an outer cap, which is why C-021 alone is not enough.
    ///
    /// *Red-reachability:* remove `.timeout(hardening.outer_cap)` from
    /// `build_index_http_client` and this test hangs past the cap.
    #[tokio::test]
    async fn a_dribbling_peer_is_ended_by_the_outer_cap() {
        // As above, a ratio rather than absolute values: `interval` must stay
        // clear of `idle_bound` (150 ms of scheduling slack, against the old
        // 200/500 pair's 300 ms) so only `outer_cap` can end the fetch, and
        // `outer_cap` is what the run actually waits out — the whole cost of
        // the test.
        let idle_bound = Duration::from_millis(180);
        let outer_cap = Duration::from_millis(400);
        let endpoint = StubIndexEndpoint::start(vec![Reply::DribbleForever {
            // Comfortably under the idle bound, so the idle bound never fires.
            interval: Duration::from_millis(30),
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

// ── Diagnostic-surface guards ─────────────────────────────────

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
            .find("// ── Source ──")
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
