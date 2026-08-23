// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{auth, oci};

use super::Client;
use super::MirrorMap;
use super::native_transport::NativeTransport;

/// Largest upload request body a registry is known to accept.
///
/// GHCR rejects any single blob-upload request whose body exceeds 4 MiB with
/// `416 REQUESTED_RANGE_NOT_SATISFIABLE` — "the request body is too large and
/// exceeds the maximum permissible limit of 4.00MiB". Every chunk ocx sends must
/// stay at or under this, or layers above the cap become unpublishable on GHCR.
pub(crate) const MAX_UPLOAD_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Body size of one chunked-push `PATCH`.
///
/// A blob at or under this goes up in a single request; anything larger is split
/// into this many bytes per `PATCH`, each carrying its own `Content-Range`, with
/// the whole-blob digest committed by the final `PUT`. Held below
/// [`MAX_UPLOAD_REQUEST_BYTES`] with a 1 MiB margin so transfer framing cannot
/// push a request over the registry's cap. Independent of the 128 KiB
/// progress-frame size in `native_transport::progress_body_stream` — that governs
/// progress granularity, this governs request size.
pub(crate) const PUSH_CHUNK_SIZE: usize = MAX_UPLOAD_REQUEST_BYTES - 1024 * 1024;

/// Bound on every registry HTTP request, mapped to
/// [`reqwest::ClientBuilder::read_timeout`].
///
/// # Two semantics, not one
///
/// The name `read_timeout` undersells it. reqwest creates the sleep at dispatch
/// and resets it only per *response-body* frame, so one value governs two
/// different things:
///
/// - **Per-frame idle bound** on the response body. An honest slow download is
///   unaffected however long it runs, as long as frames keep arriving; a body
///   that goes quiet for this long is treated as dead.
/// - **Hard deadline** on everything before the first body frame: connect, TLS
///   handshake, request-body upload, the whole redirect chain, and the
///   registry's time-to-first-response-byte. None of these reset the sleep.
///
/// # Why 120 s
///
/// The goal is a bounded hang, not a tight SLA — the fork defaults this to
/// `None`, which hangs forever, and nothing on the pull path retries
/// (`pull_local.rs` propagates with `?`). So the value only has to be small
/// enough to fail eventually and large enough that no honest transfer trips it.
///
/// The binding constraint is the *upload* half, where the deadline is hard: a
/// [`PUSH_CHUNK_SIZE`] (3 MiB) `PATCH` must complete inside it, which at 120 s
/// needs ~26 KiB/s of sustained uplink — below any link that could publish at
/// all. At 30 s the same chunk demands ~105 KiB/s, which a domestic uplink or a
/// throttled CI runner misses. The empty-body committing `PUT ?digest=` is the
/// other case: the registry does its whole-blob commit server-side before
/// answering, and that wait is time-to-first-byte, i.e. deadline, not idle.
/// 120 s sits in the same neighbourhood as containerd's 5-minute default.
///
/// # Relation to the pull-path drain
///
/// Bounded-hang complement to the trailer drain in `Client::pull_layer`: a
/// registry that stalls mid-body — including after the tar's end-of-archive
/// marker but before the drain has pulled the codec trailer — now surfaces an
/// error instead of blocking the pull forever. Mid-download it lands as
/// [`ClientError::ShortBlobRead`](super::error::ClientError::ShortBlobRead) via
/// the completeness discriminator: `TempFail` (75), retryable, which is the
/// correct taxonomy for a transport that went quiet.
pub(crate) const REGISTRY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Bound on the connect phase of every registry request, mapped to
/// [`reqwest::ClientBuilder::connect_timeout`].
///
/// The fork defaults this to `None`, which is unbounded: a host that accepts
/// nothing and refuses nothing — a black-holing firewall, a load balancer with
/// no backend — leaves the socket in SYN_SENT until the OS gives up, which on
/// Linux is over two minutes and on some tunings far longer.
/// [`REGISTRY_READ_TIMEOUT`] does not cover this, because a connect that never
/// completes never dispatches a request for reqwest to time.
///
/// 30 s matches `INDEX_CONNECT_TIMEOUT` in `oci/index/ocx_index.rs`, the
/// sibling bound on the index-document fetch: both are the connect phase
/// against a registry-class endpoint, and one number is easier to reason about
/// than two.
pub(crate) const REGISTRY_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub struct ClientBuilder {
    auth: auth::Auth,
    config: oci::native::ClientConfig,
    lock_timeout: Option<std::time::Duration>,
    progress: crate::cli::progress::ProgressManager,
    mirrors: MirrorMap,
}

impl ClientBuilder {
    pub fn new() -> Self {
        ClientBuilder {
            auth: auth::Auth::default(),
            config: oci::native::ClientConfig {
                push_chunk_size: PUSH_CHUNK_SIZE,
                read_timeout: Some(REGISTRY_READ_TIMEOUT),
                connect_timeout: Some(REGISTRY_CONNECT_TIMEOUT),
                // CA roots are seeded by the fork's `ClientConfig::default()`
                // (self-contained on hosts with no system trust store), inherited
                // here via `..Default::default()`. Single source of truth: the fork.
                ..Default::default()
            },
            lock_timeout: None,
            progress: crate::cli::progress::ProgressManager::disabled(),
            mirrors: MirrorMap::default(),
        }
    }

    /// Sets the shared progress manager used for download/upload bars.
    pub fn progress(mut self, progress: crate::cli::progress::ProgressManager) -> Self {
        self.progress = progress;
        self
    }

    /// Registries that should be contacted over plain HTTP instead of HTTPS.
    pub fn plain_http_registries(mut self, registries: Vec<String>) -> Self {
        if !registries.is_empty() {
            self.config.protocol = oci::native::ClientProtocol::HttpsExcept(registries);
        }
        self
    }

    /// Per-upstream-host registry mirror map, applied on the read path.
    ///
    /// The builder takes an already-parsed [`MirrorMap`] (parsing and the
    /// `url`-required validation happen in the config layer), mirroring the
    /// [`plain_http_registries`](Self::plain_http_registries) precedent of
    /// taking parsed primitives rather than the `Config` tree.
    pub fn mirrors(mut self, mirrors: MirrorMap) -> Self {
        self.mirrors = mirrors;
        self
    }

    /// Timeout for acquiring file locks during install and status checks.
    pub fn lock_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.lock_timeout = Some(timeout);
        self
    }

    /// Pins every connection through an SSRF [`GuardedResolver`](crate::oci::ssrf::GuardedResolver)
    /// carrying `trusted_hosts`.
    ///
    /// Used for the physical-fetch client of an index source, whose target host
    /// comes from remote-controlled root `repository` pointers: the resolver
    /// re-validates the resolved addresses at connect time (resolve -> validate
    /// -> pin), so the address a pre-flight approved cannot rebind to a forbidden
    /// range before the socket opens. Hosts / CIDRs in `trusted_hosts` skip
    /// validation (the private-registry escape hatch).
    pub fn ssrf_guard(mut self, trusted_hosts: Vec<String>) -> Self {
        let resolver: std::sync::Arc<dyn reqwest::dns::Resolve> = std::sync::Arc::new(
            crate::oci::ssrf::GuardedResolver::new(std::sync::Arc::new(trusted_hosts)),
        );
        self.config.dns_resolver = Some(resolver);
        self
    }

    pub fn build(self) -> Client {
        let transport = NativeTransport::new(oci::native::Client::new(self.config), self.auth);
        Client {
            transport: Box::new(transport),
            lock_timeout: self.lock_timeout.unwrap_or(std::time::Duration::from_secs(30)),
            tag_chunk_size: 100,
            repository_chunk_size: 100,
            progress: self.progress,
            mirrors: self.mirrors,
        }
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl ClientBuilder {
    /// Test-only inspector: the resolved plain-HTTP host list the transport
    /// will be configured with.
    ///
    /// Returns `Some(hosts)` when [`plain_http_registries`](Self::plain_http_registries)
    /// set the protocol to `HttpsExcept(hosts)`, `None` otherwise (default
    /// HTTPS-everywhere). Exposed only under `#[cfg(test)]` so the F2 unit test
    /// can prove a mirror host actually reaches the plain-HTTP transport config
    /// — it is **not** part of the production API. The protocol field is
    /// consumed by [`build`](Self::build) into the transport's `oci::native::Client`,
    /// which exposes no public getter, so this is the only seam that can observe
    /// the resolved value before construction.
    pub(crate) fn plain_http_hosts(&self) -> Option<&[String]> {
        match &self.config.protocol {
            oci::native::ClientProtocol::HttpsExcept(hosts) => Some(hosts.as_slice()),
            _ => None,
        }
    }

    /// Test-only inspector: how many bundled CA roots were seeded into the
    /// client config. Non-zero proves the trust store is self-contained and the
    /// empty-system-store panic path cannot be reached.
    pub(crate) fn root_certificate_count(&self) -> usize {
        self.config.extra_root_certificates.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: every client ships with the full bundled Mozilla CA root
    /// set, DER-encoded, so TLS works without a system trust store.
    ///
    /// The reported crash was a panic — `No CA certificates were loaded from the
    /// system` — when reqwest's rustls path (0.13) found an empty system store
    /// and `Client::new` "fell back" to `reqwest::Client::default()`, whose
    /// internal `.expect()` re-triggered the identical failure.
    ///
    /// That empty-store state cannot be reproduced deterministically in-process:
    /// `rustls-native-certs` discovers the store via `openssl_probe::probe()`,
    /// which *unconditionally* re-adds any existing system cert directory
    /// (`/etc/ssl/certs`, …), so `SSL_CERT_FILE` / `SSL_CERT_DIR` overrides can
    /// never make it empty on a host that has certs — every dev box and CI runner
    /// does. What *is* assertable host-independently is the fix's invariant: the
    /// config the whole builder funnels through carries the complete root set,
    /// which forces reqwest onto the `Verifier::new_with_extra_roots` branch that
    /// never errors on an empty store.
    #[test]
    fn new_seeds_full_der_encoded_ca_root_set() {
        let builder = ClientBuilder::new();

        // Fix present, and the *entire* Mozilla set — guards against a future
        // change that truncates the list to a stray cert or two.
        assert_eq!(
            builder.root_certificate_count(),
            webpki_root_certs::TLS_SERVER_ROOT_CERTS.len(),
            "every bundled Mozilla root must be seeded into the client config"
        );
        assert!(
            builder.root_certificate_count() > 100,
            "the Mozilla root set should be well over 100 certificates, got {}",
            builder.root_certificate_count()
        );

        // Constructing the client runs `convert_certificates` → reqwest's
        // `Certificate::from_der` over every seeded root; reaching this line
        // proves they are valid DER. A wrong encoding tag (PEM over DER bytes)
        // would fail conversion and trip the very `Client::new` panic under test.
        let _client = builder.build();
    }

    /// `http://` mirror host flows to `plain_http_registries`.
    ///
    /// When a mirror's URL uses `http://`, its host must reach the client's
    /// plain-HTTP transport config so connections succeed without TLS. Per the
    /// Living Design Record, composition is **explicit** — the operator lists
    /// the mirror host in `OCX_INSECURE_REGISTRIES`; the `http://` scheme alone
    /// never opts out of TLS. This test proves the host actually lands in the
    /// resolved `HttpsExcept` transport list (via the `#[cfg(test)]`
    /// `plain_http_hosts` inspector), not merely that the MirrorMap carries it.
    ///
    /// Traces: plan Testing Strategy — "`http://` mirror host flows to
    /// `plain_http_registries`"; ADR F2.
    #[test]
    fn http_mirror_host_flows_to_plain_http_registries() {
        use crate::config::mirror::ParsedMirror;

        let mirror_map = MirrorMap::new([(
            "ghcr.io".to_string(),
            ParsedMirror {
                protocol: "http".to_string(),
                host: "local-mirror.corp:5001".to_string(),
                path_prefix: String::new(),
            },
        )]);

        let builder = ClientBuilder::new()
            .plain_http_registries(vec!["local-mirror.corp:5001".to_string()])
            .mirrors(mirror_map);

        // Prove the mirror host reached the plain-HTTP transport config — this
        // is the assertion that would FAIL if `plain_http_registries` were a
        // no-op. `scheme_for("local-mirror.corp:5001")` would then return
        // "https" and a plain-HTTP mirror would be unreachable.
        let hosts = builder
            .plain_http_hosts()
            .expect("plain_http_registries must set the protocol to HttpsExcept");
        assert!(
            hosts.iter().any(|h| h == "local-mirror.corp:5001"),
            "the http mirror host must reach the plain-HTTP transport config, got: {hosts:?}"
        );

        // And the built client still carries the mirror map (read-path rewrite).
        let client = builder.build();
        assert!(
            client.mirrors.get("ghcr.io").is_some(),
            "ghcr.io mirror entry must be present on the built client"
        );
    }
}

/// Chunked blob upload — wire-shape proof against a stub registry.
///
/// The registries that reject an oversized upload request (GHCR: 4 MiB) cannot be
/// stood up locally, and the `registry:2` test container imposes no cap at all, so
/// the guarantee is asserted on the request shape ocx emits rather than on a live
/// rejection: how many `PATCH`es a blob is split into, what `Content-Range` each
/// carries, and that the committing `PUT` names the whole-blob digest.
#[cfg(test)]
mod push_wire_tests {
    use super::*;
    use crate::oci::client::error::ClientError;
    use crate::oci::client::transport::{OciTransport as _, no_progress};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        target: String,
        content_range: Option<String>,
        body_len: usize,
    }

    /// Injected chunk-`PATCH` failures: answer the next `remaining` of them
    /// with `status` instead of `202`.
    ///
    /// The budget is shared across **connections**, not per-connection. A
    /// restarted upload opens a fresh session and may well arrive on a fresh
    /// socket, so a per-connection counter would fault every attempt equally —
    /// the test could then never distinguish "retried and recovered" from
    /// "never retried", which is the whole thing it exists to show.
    #[derive(Clone)]
    struct PatchFaults {
        status: u16,
        remaining: Arc<AtomicUsize>,
    }

    impl PatchFaults {
        fn none() -> Self {
            Self::new(0, 0)
        }

        fn new(status: u16, count: usize) -> Self {
            Self {
                status,
                remaining: Arc::new(AtomicUsize::new(count)),
            }
        }

        /// Spends one unit of budget, returning the status to answer with;
        /// `None` once the budget is exhausted.
        fn take(&self) -> Option<u16> {
            self.remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                .ok()
                .map(|_| self.status)
        }
    }

    /// Minimal HTTP/1.1 stub of an OCI blob-upload session.
    ///
    /// Speaks only the four requests a push makes — `HEAD` (blob absent), `POST`
    /// (open session), `PATCH` (one chunk), `PUT` (commit) — and records each one.
    /// Answers every `PATCH` with a `Range` header reporting cumulative progress,
    /// which is what a real registry does and what the client is required to trust
    /// over its own offsets.
    struct StubRegistry {
        address: String,
        recorded: Arc<Mutex<Vec<RecordedRequest>>>,
    }

    impl StubRegistry {
        /// `accepted_first_chunk` overrides the byte count reported for the first
        /// `PATCH`, simulating a registry that stored less than it was sent.
        async fn start(accepted_first_chunk: Option<usize>) -> Self {
            Self::start_with(accepted_first_chunk, PatchFaults::none()).await
        }

        async fn start_with(accepted_first_chunk: Option<usize>, faults: PatchFaults) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let recorded = Arc::new(Mutex::new(Vec::new()));

            let log = Arc::clone(&recorded);
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let log = Arc::clone(&log);
                    let faults = faults.clone();
                    tokio::spawn(async move {
                        serve_connection(socket, log, accepted_first_chunk, faults).await;
                    });
                }
            });

            Self { address, recorded }
        }

        fn recorded(&self) -> Vec<RecordedRequest> {
            self.recorded.lock().unwrap().clone()
        }

        fn method(&self, method: &str) -> Vec<RecordedRequest> {
            self.recorded().into_iter().filter(|r| r.method == method).collect()
        }

        fn posts(&self) -> Vec<RecordedRequest> {
            self.method("POST")
        }

        fn patches(&self) -> Vec<RecordedRequest> {
            self.method("PATCH")
        }

        fn puts(&self) -> Vec<RecordedRequest> {
            self.method("PUT")
        }
    }

    async fn serve_connection(
        socket: tokio::net::TcpStream,
        recorded: Arc<Mutex<Vec<RecordedRequest>>>,
        accepted_first_chunk: Option<usize>,
        faults: PatchFaults,
    ) {
        let (read_half, mut write_half) = socket.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);
        let mut stored: usize = 0;
        let mut patch_count: usize = 0;

        loop {
            let mut request_line = String::new();
            match reader.read_line(&mut request_line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let mut parts = request_line.split_whitespace();
            let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
                return;
            };
            let (method, target) = (method.to_string(), target.to_string());

            let mut content_length = 0usize;
            let mut chunked = false;
            let mut content_range = None;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                    return;
                }
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let Some((name, value)) = header.split_once(':') else {
                    continue;
                };
                let value = value.trim();
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => content_length = value.parse().unwrap_or(0),
                    "transfer-encoding" if value.eq_ignore_ascii_case("chunked") => chunked = true,
                    "content-range" => content_range = Some(value.to_string()),
                    _ => {}
                }
            }

            let body_len = if chunked {
                read_chunked_body(&mut reader).await
            } else {
                let mut body = vec![0u8; content_length];
                reader.read_exact(&mut body).await.unwrap();
                content_length
            };

            recorded.lock().unwrap().push(RecordedRequest {
                method: method.clone(),
                target: target.clone(),
                content_range,
                body_len,
            });

            let response = match method.as_str() {
                // Deliberately a *relative* Location — a registry may answer either
                // way, and the client must resolve it against the request host.
                "POST" => {
                    // A POST opens a *fresh* upload session, so what an abandoned
                    // session stored is forgotten — a real registry behaves this way,
                    // and it is what makes a restart-style retry safe.
                    stored = 0;
                    patch_count = 0;
                    "HTTP/1.1 202 Accepted\r\nLocation: /v2/test/blob/blobs/uploads/stub\r\nContent-Length: 0\r\n\r\n"
                        .to_string()
                }
                // A faulted PATCH answers before the accounting: the bytes it
                // carried were never stored, so `stored` must not advance.
                "PATCH" => match faults.take() {
                    Some(status) => format!("HTTP/1.1 {status} Injected Fault\r\nContent-Length: 0\r\n\r\n"),
                    None => {
                        stored += body_len;
                        patch_count += 1;
                        let reported = match accepted_first_chunk {
                            Some(accepted) if patch_count == 1 => accepted,
                            _ => stored,
                        };
                        format!(
                            "HTTP/1.1 202 Accepted\r\nLocation: /v2/test/blob/blobs/uploads/stub\r\nRange: 0-{}\r\nContent-Length: 0\r\n\r\n",
                            reported.saturating_sub(1)
                        )
                    }
                },
                "PUT" => format!("HTTP/1.1 201 Created\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n"),
                // HEAD (blob_exists) and anything else: not present.
                _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string(),
            };
            if write_half.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    async fn read_chunked_body(reader: &mut tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>) -> usize {
        let mut total = 0usize;
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).await.unwrap_or(0) == 0 {
                return total;
            }
            let size_field = size_line.trim().split(';').next().unwrap_or("0");
            let size = usize::from_str_radix(size_field, 16).unwrap_or(0);
            if size == 0 {
                // Consume the terminating CRLF of the last-chunk marker.
                let mut trailer = String::new();
                let _ = reader.read_line(&mut trailer).await;
                return total;
            }
            let mut chunk = vec![0u8; size + 2];
            reader.read_exact(&mut chunk).await.unwrap();
            total += size;
        }
    }

    /// The stub's repository reference, built by parsing rather than by the
    /// direct constructors: those are seam-restricted to the mirror-routing files
    /// (`native_reference_direct_construction_restricted_to_seams`), and a stub
    /// fixture is not a new read path.
    fn stub_image(stub: &StubRegistry) -> oci::native::Reference {
        format!("{}/test/blob:latest", stub.address).parse().unwrap()
    }

    fn sha256_digest(data: &[u8]) -> crate::oci::Digest {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(data);
        crate::oci::Digest::Sha256(hex::encode(hasher.finalize()))
    }

    /// Pushes through the transport the production [`ClientBuilder`] builds, so the
    /// shipped `push_chunk_size` — not a value the test picked — drives the wire.
    async fn push_through_production_client(
        stub: &StubRegistry,
        data: Vec<u8>,
    ) -> super::super::transport::Result<String> {
        let config = ClientBuilder::new()
            .plain_http_registries(vec![stub.address.clone()])
            .config;
        let transport = NativeTransport::new(oci::native::Client::new(config), auth::Auth::default());
        let image = stub_image(stub);
        let digest = sha256_digest(&data);
        transport.push_blob(&image, data, &digest, no_progress()).await
    }

    /// The shipped chunk size must never exceed what the strictest known registry
    /// accepts in one request. A "raise it for throughput" commit that walks past
    /// GHCR's cap re-breaks every layer over 4 MiB — this is the tripwire.
    #[test]
    fn shipped_chunk_size_stays_within_the_registry_request_cap() {
        let configured = ClientBuilder::new().config.push_chunk_size;
        assert!(
            configured > 0,
            "a zero chunk size would never terminate the upload loop"
        );
        assert!(
            configured <= MAX_UPLOAD_REQUEST_BYTES,
            "push_chunk_size {configured} exceeds the {MAX_UPLOAD_REQUEST_BYTES}-byte per-request cap GHCR enforces"
        );
    }

    /// A blob larger than the chunk size goes up as several `PATCH`es whose
    /// `Content-Range` values tile the blob exactly once, followed by one `PUT`
    /// carrying the digest of the *whole* blob.
    #[tokio::test]
    async fn blob_over_the_chunk_size_uploads_as_contiguous_chunks() {
        let stub = StubRegistry::start(None).await;
        let data: Vec<u8> = (0..PUSH_CHUNK_SIZE * 2 + 1024).map(|i| (i % 251) as u8).collect();
        // The digest travels as a query parameter, so `:` arrives percent-encoded —
        // match on the hex, which is unambiguous either way.
        let expected_digest = sha256_digest(&data).hex().to_string();
        let total = data.len();

        push_through_production_client(&stub, data).await.unwrap();

        let patches = stub.patches();
        assert_eq!(
            patches.len(),
            3,
            "a blob of {total} bytes must be split into 3 chunks, got {patches:?}"
        );

        let mut next_start = 0usize;
        for patch in &patches {
            let range = patch
                .content_range
                .as_deref()
                .expect("every chunk PATCH must carry Content-Range");
            let (start, end) = range.split_once('-').expect("Content-Range must be `start-end`");
            let (start, end) = (start.parse::<usize>().unwrap(), end.parse::<usize>().unwrap());
            assert_eq!(
                start, next_start,
                "chunk ranges must be contiguous, got {range} after {next_start}"
            );
            assert_eq!(
                end - start + 1,
                patch.body_len,
                "Content-Range {range} must describe exactly the {} bytes sent",
                patch.body_len
            );
            assert!(
                patch.body_len <= MAX_UPLOAD_REQUEST_BYTES,
                "a {}-byte request body would be rejected by GHCR",
                patch.body_len
            );
            next_start = end + 1;
        }
        assert_eq!(next_start, total, "the chunks must cover the whole blob exactly once");

        let puts = stub.puts();
        assert_eq!(puts.len(), 1, "the session must be committed by exactly one PUT");
        assert!(
            puts[0].target.contains(&expected_digest),
            "the committing PUT must name the whole-blob digest {expected_digest}, got {}",
            puts[0].target
        );
        assert_eq!(
            puts[0].body_len, 0,
            "the committing PUT carries no body — the bytes are already stored"
        );
    }

    /// A blob at or under the chunk size goes up in a single body-bearing request.
    #[tokio::test]
    async fn blob_under_the_chunk_size_uploads_in_one_request() {
        let stub = StubRegistry::start(None).await;
        let data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();

        push_through_production_client(&stub, data).await.unwrap();

        let patches = stub.patches();
        assert_eq!(patches.len(), 1, "a 1 KiB blob must not be chunked, got {patches:?}");
        assert_eq!(patches[0].content_range.as_deref(), Some("0-1023"));
        assert_eq!(patches[0].body_len, 1024);
        assert_eq!(stub.puts().len(), 1, "the session must still be committed by one PUT");
    }

    /// A registry that reports storing fewer bytes than it was sent must stop a
    /// too-large blob's upload dead — no commit, and no fallback either.
    ///
    /// Continuing from the client's own offsets would leave a gap in the stored blob
    /// and then name it with the digest of the bytes that were *sent* — a blob whose
    /// content does not match its own name. The buffered fallback is no escape: it
    /// ends in a single `PUT` carrying the whole blob, which above the per-request
    /// cap is the `416` this chunking exists to avoid, reached through another door.
    /// Asserted on the production transport path, which is what ships.
    #[tokio::test]
    async fn over_cap_blob_short_accepted_never_reaches_a_committing_put() {
        let stub = StubRegistry::start(Some(64)).await;
        let data: Vec<u8> = (0..MAX_UPLOAD_REQUEST_BYTES + 1024).map(|i| (i % 251) as u8).collect();

        let error = push_through_production_client(&stub, data)
            .await
            .expect_err("a registry that stored 64 of the bytes sent must not yield a successful push");

        assert!(
            matches!(error, ClientError::Registry(_)),
            "the spec violation must propagate as a registry failure, got {error:?}"
        );
        assert!(
            stub.puts().is_empty(),
            "no PUT may commit a blob the registry only partially stored, saw {:?}",
            stub.puts()
        );
    }

    /// A blob that still fits in one request may fall back after a short accept —
    /// and the fallback must re-push from the start, so the committing `PUT` carries
    /// every byte.
    ///
    /// That is the whole reason the fallback is safe here: the partial bytes sit
    /// under an abandoned session, and the `PUT` that finally names the digest
    /// carries the entire blob rather than resuming on top of bytes nobody can
    /// account for. A fallback that resumed — or one dropped altogether — breaks
    /// this.
    #[tokio::test]
    async fn under_cap_fallback_commits_the_whole_blob_in_one_request() {
        let stub = StubRegistry::start(Some(64)).await;
        let data: Vec<u8> = (0..PUSH_CHUNK_SIZE + 1024).map(|i| (i % 251) as u8).collect();
        let total = data.len();
        assert!(
            total <= MAX_UPLOAD_REQUEST_BYTES,
            "fixture must stay under the per-request cap for the fallback to be reachable"
        );

        push_through_production_client(&stub, data)
            .await
            .expect("a blob that fits in one request must still publish via the fallback");

        let puts = stub.puts();
        assert_eq!(puts.len(), 1, "exactly one PUT may commit the blob, saw {puts:?}");
        assert_eq!(
            puts[0].body_len, total,
            "the committing PUT must carry the whole blob — a resuming fallback would commit bytes the registry never stored"
        );
    }

    /// A 503 on a chunk `PATCH` must not lose the layer: the push restarts from
    /// a fresh session and still commits the blob exactly once.
    ///
    /// The restart is whole-blob by design — a fresh `POST` abandons whatever
    /// the failed session stored, which is why the recovery is safe without any
    /// range reconciliation. The assertions pin that shape rather than just the
    /// `Ok`: two `POST`s (a *new* session, not a resume on the old one), the two
    /// surviving `PATCH`es tiling the blob exactly once, and one `PUT`.
    #[tokio::test]
    async fn transient_patch_failure_restarts_the_upload_and_commits_once() {
        let stub = StubRegistry::start_with(None, PatchFaults::new(503, 1)).await;
        let data: Vec<u8> = (0..PUSH_CHUNK_SIZE + 1024).map(|i| (i % 251) as u8).collect();
        let total = data.len();

        push_through_production_client(&stub, data)
            .await
            .expect("a 503 on one chunk must be retried, not surfaced as a failed push");

        assert_eq!(
            stub.posts().len(),
            2,
            "the retry must open a FRESH upload session, not resume the failed one; saw {:?}",
            stub.posts()
        );

        let patches = stub.patches();
        assert_eq!(
            patches.len(),
            3,
            "one faulted PATCH plus the two of the successful attempt; saw {patches:?}"
        );

        let mut next_start = 0usize;
        for patch in &patches[1..] {
            let range = patch
                .content_range
                .as_deref()
                .expect("every chunk PATCH must carry Content-Range");
            let (start, end) = range.split_once('-').expect("Content-Range must be `start-end`");
            let (start, end) = (start.parse::<usize>().unwrap(), end.parse::<usize>().unwrap());
            assert_eq!(
                start, next_start,
                "the retried attempt's chunks must be contiguous, got {range} after {next_start}"
            );
            next_start = end + 1;
        }
        assert_eq!(
            next_start, total,
            "the retried attempt must cover the whole blob exactly once"
        );

        let puts = stub.puts();
        assert_eq!(puts.len(), 1, "the blob must be committed exactly once, saw {puts:?}");
    }

    /// A registry that faults *every* chunk spends the whole budget and then
    /// surfaces the fault as transient — the exit-75 contract, not a 69.
    ///
    /// Two things are pinned that the recovering sibling above cannot show:
    /// the budget is bounded (exactly three `POST`s — one initial attempt plus
    /// `PUSH_RETRY_ATTEMPTS` restarts, so a raised constant reds this), and
    /// exhausting it does not downgrade the error. A caller that reruns on 75
    /// is exactly right here: the registry never answered usefully.
    #[tokio::test]
    async fn exhausted_transient_retries_surface_as_transient() {
        let stub = StubRegistry::start_with(None, PatchFaults::new(503, usize::MAX)).await;
        let data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();

        let error = push_through_production_client(&stub, data)
            .await
            .expect_err("a registry that faults every chunk must not yield a successful push");

        assert_eq!(
            stub.posts().len(),
            3,
            "the budget is one initial attempt plus PUSH_RETRY_ATTEMPTS restarts; saw {:?}",
            stub.posts()
        );
        assert!(
            matches!(error, ClientError::RegistryTransient(_)),
            "an exhausted transient retry must stay transient (exit 75), got {error:?}"
        );
    }

    /// A 401 is not transient, so it must be surfaced on the first attempt.
    ///
    /// One `POST` is the discriminator: a retry-everything loop would open
    /// three sessions before giving up, spending two extra round trips and two
    /// backoff waits on an answer that will not change.
    #[tokio::test]
    async fn permanent_patch_failure_is_not_retried() {
        let stub = StubRegistry::start_with(None, PatchFaults::new(401, usize::MAX)).await;
        let data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();

        let error = push_through_production_client(&stub, data)
            .await
            .expect_err("a registry that rejects every chunk must not yield a successful push");

        assert_eq!(
            stub.posts().len(),
            1,
            "a credentials rejection must not be retried; saw {:?}",
            stub.posts()
        );
        assert!(
            matches!(error, ClientError::Authentication(_)),
            "a 401 on a chunk PATCH must surface as Authentication (80), got {error:?}"
        );
    }
}

/// Registry read timeout — a registry that goes quiet mid-body, or never answers
/// at all, must not hang the pull.
///
/// The fork defaults `read_timeout` to `None`, so before this bound a stalled
/// response body blocked forever. Nothing above the transport can recover from
/// that: the drain in `Client::pull_layer` is itself a read, so a stall between
/// the tar's end-of-archive marker and the codec trailer hangs *inside* the
/// bounded-hang fix rather than around it.
#[cfg(test)]
mod read_timeout_tests {
    use super::*;
    use crate::oci::client::error::ClientError;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    /// A registry that answers the handshake, starts a blob body, then goes
    /// silent without closing the connection.
    ///
    /// Silent-but-open is the case a timeout is needed for at all — a close
    /// yields EOF, which every layer above already handles. The declared
    /// `Content-Length` deliberately exceeds what is written, so the client has
    /// every reason to keep waiting.
    async fn start_stalling_registry() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();

        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (read_half, mut write_half) = socket.into_split();
                    let mut reader = tokio::io::BufReader::new(read_half);

                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    // Drain headers up to the blank line.
                    loop {
                        let mut header = String::new();
                        match reader.read_line(&mut header).await {
                            Ok(0) | Err(_) => return,
                            Ok(_) if header.trim_end().is_empty() => break,
                            Ok(_) => {}
                        }
                    }

                    let target = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
                    if !target.contains("/blobs/") {
                        // Auth ping and anything else: answer normally so the
                        // request under test is the blob body, not the handshake.
                        let _ = write_half
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await;
                        return;
                    }

                    // Promise far more than is delivered, hand over a few bytes,
                    // then stall while holding the socket open.
                    let _ = write_half
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\nstalled")
                        .await;
                    let _ = write_half.flush().await;
                    std::future::pending::<()>().await;
                });
            }
        });

        address
    }

    /// Reads a blob body from `address` through the real `NativeTransport`,
    /// with `read_timeout` overridden so the test does not wait out the shipped
    /// 30 s. The override rides the same `.config` field access
    /// `push_through_production_client` already uses — no new knob.
    async fn read_stalled_blob_with_timeout(address: &str, read_timeout: Duration) -> std::io::Result<u64> {
        let mut config = ClientBuilder::new()
            .plain_http_registries(vec![address.to_string()])
            .config;
        config.read_timeout = Some(read_timeout);

        let transport = NativeTransport::new(oci::native::Client::new(config), auth::Auth::default());
        let image: oci::native::Reference = format!("{address}/test/blob:latest").parse().unwrap();
        let digest = crate::oci::Digest::Sha256("a".repeat(64));

        let mut stream = crate::oci::client::transport::OciTransport::pull_blob_streaming(&transport, &image, &digest)
            .await
            .expect("the stub answers the blob GET, so opening the stream must succeed");
        tokio::io::copy(&mut stream, &mut tokio::io::sink()).await
    }

    /// The shipped client must carry a read timeout. `None` — the fork's default
    /// — is an unbounded hang, and it is invisible until a registry stalls in
    /// production, which is exactly when nobody is looking at this file.
    #[test]
    fn production_client_config_carries_the_registry_read_timeout() {
        assert_eq!(
            ClientBuilder::new().config.read_timeout,
            Some(REGISTRY_READ_TIMEOUT),
            "ClientBuilder::new() must set read_timeout — inheriting the fork's None means a stalled registry hangs the pull forever"
        );
    }

    /// The connect phase needs its own bound: a host that neither accepts nor
    /// refuses the SYN never dispatches a request, so the read timeout — which
    /// reqwest arms at dispatch — never gets a chance to fire.
    #[test]
    fn production_client_config_carries_the_registry_connect_timeout() {
        assert_eq!(
            ClientBuilder::new().config.connect_timeout,
            Some(REGISTRY_CONNECT_TIMEOUT),
            "ClientBuilder::new() must set connect_timeout — inheriting the fork's None means a black-holed connect hangs until the OS gives up"
        );
    }

    /// End-to-end proof that the bound actually fires: a stalled body read
    /// returns an error rather than blocking.
    ///
    /// The injected 2 s is not only the idle bound — it is also the deadline the
    /// stub's *response head* must beat (see the const's "two semantics"). A
    /// value tight enough to be fast is therefore a value an oversubscribed
    /// runner can trip before the test reaches its real assertion, surfacing as
    /// a misdirecting failure on `pull_blob_streaming`. 2 s clears that with
    /// room; the outer guard stays 5× it.
    #[tokio::test]
    async fn stalled_response_body_read_returns_instead_of_hanging() {
        let address = start_stalling_registry().await;

        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            read_stalled_blob_with_timeout(&address, Duration::from_secs(2)),
        )
        .await
        .expect("a read timeout must end the stalled read — timing out here means the client hung");

        let error = outcome.expect_err("a body that never completes must not read as a successful copy");
        // reqwest wraps the timeout, so the outer `io::ErrorKind` is `Other`.
        // Ask reqwest's own predicate rather than matching on the message.
        let timed_out = std::iter::successors(std::error::Error::source(&error), |current| {
            std::error::Error::source(*current)
        })
        .any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
        });
        assert!(
            timed_out,
            "the stall must surface as a timeout somewhere in the source chain, got {error:?}"
        );
    }

    /// The timeout must not only fire, it must land in the retryable bucket:
    /// a registry that went quiet exits 75, not 69.
    ///
    /// This is the only way to exercise `registry_error`'s `is_timeout()` arm —
    /// `reqwest::Error` has no public constructor, so a real stalled socket is
    /// the sole source of one. `pull_blob` is the seam because it buffers the
    /// body through the fork, which converts the stalled read into
    /// `OciDistributionError::RequestError` before ocx maps it.
    #[tokio::test]
    async fn stalled_registry_read_timeout_classifies_as_transient() {
        let address = start_stalling_registry().await;

        let mut config = ClientBuilder::new().plain_http_registries(vec![address.clone()]).config;
        config.read_timeout = Some(Duration::from_secs(2));
        let transport = NativeTransport::new(oci::native::Client::new(config), auth::Auth::default());
        let image: oci::native::Reference = format!("{address}/test/blob:latest").parse().unwrap();
        let digest = crate::oci::Digest::Sha256("a".repeat(64));

        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            crate::oci::client::transport::OciTransport::pull_blob(&transport, &image, &digest),
        )
        .await
        .expect("a read timeout must end the stalled pull — timing out here means the client hung");

        let error = outcome.expect_err("a body that never completes must not read as a successful pull");
        assert!(
            matches!(error, ClientError::RegistryTransient(_)),
            "a stalled registry must classify as RegistryTransient (TempFail/75), got {error:?}"
        );
    }
}
