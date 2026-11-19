// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The HTTP client the export sends through: ocx's own, never the crate's.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use http::header::{CONTENT_ENCODING, CONTENT_TYPE, USER_AGENT};
use http::{HeaderName, HeaderValue};
use ocx_oci::ssrf::{DialScheme, GuardedResolver, ProxyChoice, ProxyRules, Route, proxy_rules};
use ocx_util::tls::ExtraRoots;
use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
use secrecy::{ExposeSecret as _, SecretString};
use url::Host;

use crate::destination::{Destination, is_refused_address};
use crate::detached;

const USER_AGENT_VALUE: &str = concat!("ocx/", env!("CARGO_PKG_VERSION"));

/// Why an export failed, as the one word the debug line carries.
///
/// A failure never carries `reqwest::Error` or a URL: both `Display` the endpoint, which may hold a
/// credential in its query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Name lookup, connection or TLS failed, or the client could not be built.
    Connect,
    /// The deadline passed, in the endpoint lookup or in the request.
    Timeout,
    /// The collector answered with a status outside 2xx and 3xx.
    Status(u16),
    /// The collector answered with a redirect, which is never followed.
    Redirect,
    /// Every address the endpoint name resolved to is one an endpoint may never reach.
    Blocked,
    /// The request could not be assembled from the records or the destination's headers.
    Encode,
    /// The export task or a thread it runs work on panicked.
    Panic,
    /// A thread for the resource probe or the name lookup could not be started.
    Thread,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect => formatter.write_str("connect"),
            Self::Blocked => formatter.write_str("blocked"),
            Self::Timeout => formatter.write_str("timeout"),
            Self::Status(code) => write!(formatter, "status {code}"),
            Self::Redirect => formatter.write_str("redirect"),
            Self::Encode => formatter.write_str("encode"),
            Self::Panic => formatter.write_str("panic"),
            Self::Thread => formatter.write_str("thread"),
        }
    }
}

/// Where the client leaves the failure class, because the exporter flattens every error it is
/// handed into a string.
#[derive(Debug, Clone, Default)]
pub(crate) struct FailureSlot(Arc<Mutex<Option<Failure>>>);

impl FailureSlot {
    fn record(&self, failure: Failure) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(failure);
    }

    /// The class the client recorded, if it recorded one.
    pub(crate) fn take(&self) -> Option<Failure> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The client for one destination: embedded roots plus the extra roots, no redirects, a deadline
/// from the destination, and a resolver that trusts no host and answers only the proxy host this
/// process pinned. The proxy, if any, is the one the single `ProxyRules` read chose: reqwest never
/// reads the OS proxy settings itself.
///
/// Every header the OTLP exporter puts on a request is discarded by [`HttpClient::send_bytes`]:
/// the request carries `user-agent`, the destination's headers and a fixed `content-type`, so an
/// ambient `OTEL_EXPORTER_OTLP_*` header can never reach a collector the operator did not name it
/// for. The response body is never read.
pub(crate) struct Client {
    inner: reqwest::Client,
    headers: Vec<(String, SecretString)>,
    failure: FailureSlot,
    /// What reqwest asks for a name it has no pin for, kept so a test can ask it the same question.
    #[cfg(test)]
    resolver: Arc<GuardedResolver>,
}

// Names only: a header value is a credential and `Debug` output reaches logs and test failures.
impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("Client")
            .field("header_names", &names)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Builds the client for `destination`, reading the proxy settings and resolving the endpoint host
    /// (or, through a proxy, the proxy host) on detached threads.
    ///
    /// The lookup and the request share the destination's timeout: a lookup that outlives it is
    /// [`Failure::Timeout`] and the stuck thread is left behind rather than awaited, so a runtime
    /// built around this call still shuts down on time.
    ///
    /// # Errors
    ///
    /// [`Failure::Timeout`] when the proxy read, the lookup or the build uses up the deadline,
    /// [`Failure::Blocked`] when every address the endpoint name resolved to is refused,
    /// [`Failure::Thread`] or [`Failure::Panic`] when a detached thread cannot start or panics, otherwise
    /// [`Failure::Connect`] when the host cannot be resolved, the proxy is unusable or the client cannot
    /// be built.
    pub(crate) async fn new(destination: &Destination, extra_roots: &ExtraRoots) -> Result<Self, Failure> {
        Self::new_with(destination, extra_roots, proxy_rules).await
    }

    /// [`Client::new`] with the proxy rules loader supplied.
    ///
    /// First use of the loader reads the OS proxy settings, which may block: it runs on a detached
    /// thread, under the deadline. This is the only read of them: `build` hands reqwest the answer and
    /// turns reqwest's own read off.
    async fn new_with<Load>(destination: &Destination, extra_roots: &ExtraRoots, load: Load) -> Result<Self, Failure>
    where
        Load: FnOnce() -> Arc<ProxyRules> + Send + 'static,
    {
        let rules = match tokio::time::timeout(destination.timeout, detached::run("ocx-telemetry-proxy", load)).await {
            Ok(Ok(rules)) => rules,
            Ok(Err(failure)) => return Err(failure),
            Err(_) => return Err(Failure::Timeout),
        };
        Self::build(destination, extra_roots, rules, system_lookup).await
    }

    /// [`Client::new`] with the proxy routing and the name lookup supplied.
    pub(crate) async fn build<Lookup>(
        destination: &Destination,
        extra_roots: &ExtraRoots,
        rules: Arc<ProxyRules>,
        lookup: Lookup,
    ) -> Result<Self, Failure>
    where
        Lookup: FnOnce(String, u16) -> io::Result<Vec<SocketAddr>> + Send + 'static,
    {
        let started = Instant::now();
        let endpoint = &destination.endpoint;
        let host = endpoint.host().ok_or(Failure::Connect)?;
        let port = endpoint.port_or_known_default().ok_or(Failure::Connect)?;
        // No host is trusted: a resolver miss on the endpoint name fails closed. A direct dial uses the
        // pinned override, an IP literal skips the resolver, and a proxied one asks only for the proxy.
        let trusted = Arc::new(Vec::new());
        // `no_proxy` turns off reqwest's own OS proxy read, which would be a second, unbounded one
        // inside `build()` that could disagree with the route decided here.
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT_VALUE);
        let scheme = if endpoint.scheme() == "https" {
            DialScheme::Https
        } else {
            DialScheme::Http
        };
        let dialled = match &host {
            Host::Domain(name) => (*name).to_owned(),
            Host::Ipv4(address) => address.to_string(),
            Host::Ipv6(address) => address.to_string(),
        };
        let route = rules.dial_route(scheme, &dialled, port);
        let mut addresses = Vec::new();
        let mut resolver = GuardedResolver::new(trusted, rules.clone());
        match route {
            Route::Direct => {
                if let Host::Domain(name) = &host {
                    addresses = direct_pins(name, port, destination.timeout, lookup).await?;
                    builder = builder.resolve_to_addrs(name, &addresses);
                }
            }
            // The endpoint name is the proxy's to look up, but the proxy's own name is ours: left to
            // the resolver it would run on the blocking pool, where a stuck lookup holds shutdown.
            Route::Proxied => {
                resolver = pin_proxy(resolver, &rules, (scheme, &dialled, port), destination.timeout, lookup).await?;
                if let Some(choice) = rules.proxy_choice(scheme, &dialled, port) {
                    builder = builder.proxy(reqwest_proxy(&choice)?);
                }
            }
        }
        // The gate leaves a loopback spelling to this point, where the route and the answer are known.
        if let Some(warning) = destination.loopback_warning(route, &addresses) {
            tracing::warn!(target: "ocx_telemetry", "{warning}");
        }
        let resolver = Arc::new(resolver);
        builder = builder.dns_resolver(resolver.clone());
        let remaining = destination
            .timeout
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or(Failure::Timeout)?;
        let builder = extra_roots
            .seed(ocx_util::tls::seed_embedded_roots(builder))
            .connect_timeout(remaining)
            .timeout(remaining);
        // `build()` loads the platform's CA roots, which reads the filesystem or the OS keychain and can
        // stall: on a detached thread, under what is left of the deadline.
        let inner =
            match tokio::time::timeout(remaining, detached::run("ocx-telemetry-tls", move || builder.build())).await {
                Ok(Ok(Ok(inner))) => inner,
                Ok(Ok(Err(_))) => return Err(Failure::Connect),
                Ok(Err(failure)) => return Err(failure),
                Err(_) => return Err(Failure::Timeout),
            };
        Ok(Self {
            inner,
            headers: destination.headers.clone(),
            failure: FailureSlot::default(),
            #[cfg(test)]
            resolver,
        })
    }

    /// The slot [`HttpClient::send_bytes`] writes the failure class to. Take it before handing the
    /// client to the exporter, which owns it from then on.
    pub(crate) fn failure_slot(&self) -> FailureSlot {
        self.failure.clone()
    }

    fn fail(&self, failure: Failure) -> HttpError {
        self.failure.record(failure);
        // `Failure` stays off `std::error::Error`: it is a class for the debug line, never an error
        // the CLI classifies, and the exporter reads only this text.
        failure.to_string().into()
    }
}

/// `choice` as a reqwest proxy, credentials included. Unusable proxy (for example a scheme reqwest was
/// not built for) is a failed export, never a direct dial in its place.
fn reqwest_proxy(choice: &ProxyChoice) -> Result<reqwest::Proxy, Failure> {
    let proxy = reqwest::Proxy::all(choice.uri()).map_err(|_| Failure::Connect)?;
    Ok(if let Some(header) = choice.basic_auth() {
        proxy.custom_http_auth(header)
    } else if let Some((user, password)) = choice.raw_auth() {
        proxy.basic_auth(user, password)
    } else {
        proxy
    })
}

/// `resolver` with the proxy a dial to `destination` goes through resolved and pinned, unless the
/// proxy is an IP literal or none intercepts the dial.
async fn pin_proxy<Lookup>(
    resolver: GuardedResolver,
    rules: &ProxyRules,
    (scheme, host, port): (DialScheme, &str, u16),
    within: Duration,
    lookup: Lookup,
) -> Result<GuardedResolver, Failure>
where
    Lookup: FnOnce(String, u16) -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    let Some((proxy, proxy_port)) = rules.proxy_endpoint(scheme, host, port) else {
        return Ok(resolver);
    };
    if proxy.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        return Ok(resolver);
    }
    let pinned = resolve_within(&proxy, proxy_port, within, lookup).await?;
    Ok(resolver.with_pinned(&proxy, pinned))
}

/// The addresses a direct dial to the endpoint name is pinned to: what the lookup answered, minus the
/// ones an endpoint may never reach (the refusal its IP literals get). A pinned address is dialled
/// without the resolver, so a name answering `169.254.169.254` would otherwise be dialled.
///
/// Owns the lookup as well as the filter, so the unfiltered answer never reaches the caller.
///
/// # Errors
///
/// [`Failure::Timeout`] when the lookup outlives `within`, [`Failure::Blocked`] when every answer is
/// refused, otherwise [`Failure::Connect`] when the name does not resolve.
async fn direct_pins<Lookup>(
    name: &str,
    port: u16,
    within: Duration,
    lookup: Lookup,
) -> Result<Vec<SocketAddr>, Failure>
where
    Lookup: FnOnce(String, u16) -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    let mut addresses = resolve_within(name, port, within, lookup).await?;
    addresses.retain(|address| !is_refused_address(address.ip()));
    if addresses.is_empty() {
        Err(Failure::Blocked)
    } else {
        Ok(addresses)
    }
}

/// The blocking system lookup, for a thread that is never joined.
fn system_lookup(name: String, port: u16) -> io::Result<Vec<SocketAddr>> {
    (name.as_str(), port).to_socket_addrs().map(Iterator::collect)
}

/// Runs `lookup` on a detached thread and waits at most `within` for it.
///
/// `getaddrinfo` cannot be interrupted, so the thread is abandoned at the deadline (see
/// [`detached::run`]).
async fn resolve_within<Lookup>(
    name: &str,
    port: u16,
    within: Duration,
    lookup: Lookup,
) -> Result<Vec<SocketAddr>, Failure>
where
    Lookup: FnOnce(String, u16) -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    let name = name.to_owned();
    match tokio::time::timeout(within, detached::run("ocx-telemetry-dns", move || lookup(name, port))).await {
        Ok(Ok(Ok(addresses))) if !addresses.is_empty() => Ok(addresses),
        Ok(Ok(_)) => Err(Failure::Connect),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(Failure::Timeout),
    }
}

#[async_trait]
impl HttpClient for Client {
    async fn send_bytes(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let (mut parts, body) = request.into_parts();
        // Whatever the exporter set, including its own merge of the ambient OTLP headers, goes.
        parts.headers.clear();
        for (name, value) in &self.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| self.fail(Failure::Encode))?;
            let mut value =
                HeaderValue::from_bytes(value.expose_secret().as_bytes()).map_err(|_| self.fail(Failure::Encode))?;
            value.set_sensitive(true);
            parts.headers.insert(name, value);
        }
        // Last and constant, so no selected header can replace them: ocx names itself, and the
        // body is always uncompressed protobuf.
        parts
            .headers
            .insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        parts.headers.remove(CONTENT_ENCODING);
        parts
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/x-protobuf"));

        let request =
            reqwest::Request::try_from(Request::from_parts(parts, body)).map_err(|_| self.fail(Failure::Encode))?;
        // `reqwest::Error` embeds the URL, so it is classified here and never returned.
        let response = self.inner.execute(request).await.map_err(|error| {
            self.fail(if error.is_timeout() {
                Failure::Timeout
            } else if error.is_redirect() {
                Failure::Redirect
            } else if error.is_builder() {
                Failure::Encode
            } else {
                Failure::Connect
            })
        })?;
        let status = response.status();
        if status.is_redirection() {
            self.failure.record(Failure::Redirect);
        } else if !status.is_success() {
            self.failure.record(Failure::Status(status.as_u16()));
        }
        // Dropped unpolled: a hostile collector can stream a body as long as it likes and none of
        // it is read, so it can neither fill memory nor hold the process past the deadline.
        drop(response);
        let mut answer = Response::new(Bytes::new());
        *answer.status_mut() = status;
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::log_capture::{captured, lines};
    use crate::testing::{SECRET_HEADER as SECRET, destination, stalled_lookup};

    async fn client_for(destination: &Destination) -> Client {
        Client::build(destination, &ExtraRoots::default(), ProxyRules::direct(), |_, _| {
            Err(io::Error::other("an IP literal needs no lookup"))
        })
        .await
        .expect("the client builds")
    }

    fn post(uri: &str) -> Request<Bytes> {
        Request::post(uri)
            .header("content-type", "text/plain")
            .header("content-encoding", "gzip")
            .header("user-agent", "the-crate/1.0")
            .header("x-ambient", "from OTEL_EXPORTER_OTLP_HEADERS")
            .header("authorization", "ambient-token")
            .body(Bytes::from_static(b"records"))
            .expect("request")
    }

    /// Reads one request, returning its head with the lower-cased header names.
    async fn read_request(stream: &mut TcpStream) -> String {
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1024];
        let head_end = loop {
            let read = stream.read(&mut chunk).await.expect("read");
            assert_ne!(read, 0, "the client closed before sending a head");
            received.extend_from_slice(&chunk[..read]);
            if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let head = String::from_utf8_lossy(&received[..head_end]).to_ascii_lowercase();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length: ")?.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while received.len() < head_end + length {
            let read = stream.read(&mut chunk).await.expect("read body");
            assert_ne!(read, 0, "the client closed mid-body");
            received.extend_from_slice(&chunk[..read]);
        }
        head
    }

    /// A one-shot collector answering with `reply` after reading the request; yields the request head.
    async fn collector(reply: &'static str) -> (SocketAddr, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let served = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let head = read_request(&mut stream).await;
            stream.write_all(reply.as_bytes()).await.expect("reply");
            stream.flush().await.expect("flush");
            head
        });
        (address, served)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n";

    fn header_names(head: &str) -> Vec<&str> {
        head.lines()
            .skip(1)
            .filter_map(|line| line.split_once(':').map(|(name, _)| name))
            .collect()
    }

    #[tokio::test]
    async fn client_and_destination_debug_never_print_a_header_value() {
        let destination = destination(
            "http://127.0.0.1:4318/v1/logs?api-key=query-needle#fragment-needle",
            Duration::from_secs(1),
        );
        let client = client_for(&destination).await;
        for printed in [
            format!("{client:?}"),
            format!("{destination:?}"),
            format!("{:?}", client.failure_slot()),
        ] {
            assert!(!printed.contains(SECRET), "{printed}");
            assert!(!printed.contains("blue"), "{printed}");
            for needle in ["query-needle", "fragment-needle", "api-key"] {
                assert!(!printed.contains(needle), "{needle} leaked into {printed}");
            }
        }
        let printed = format!("{client:?}");
        assert!(
            printed.contains("authorization") && printed.contains("x-team"),
            "{printed}"
        );
    }

    #[tokio::test]
    async fn request_headers_are_exactly_the_destinations() {
        let (address, served) = collector(OK).await;
        let client = client_for(&destination(
            &format!("http://{address}/v1/logs"),
            Duration::from_secs(2),
        ))
        .await;

        let response = client
            .send_bytes(post(&format!("http://{address}/v1/logs")))
            .await
            .expect("sent");
        let head = served.await.expect("collector");

        assert_eq!(response.status(), 200);
        assert_eq!(client.failure_slot().take(), None);
        let names = header_names(&head);
        for ambient in ["x-ambient", "content-encoding"] {
            assert!(!names.contains(&ambient), "{head}");
        }
        for name in &names {
            assert!(
                [
                    "host",
                    "user-agent",
                    "accept",
                    "content-type",
                    "content-length",
                    "authorization",
                    "x-team"
                ]
                .contains(name),
                "unexpected header {name}: {head}"
            );
        }
        let count = |name: &str| names.iter().filter(|candidate| **candidate == name).count();
        assert_eq!(count("content-type"), 1, "{head}");
        assert_eq!(count("user-agent"), 1, "{head}");
        assert_eq!(count("authorization"), 1, "{head}");
        assert!(head.contains("content-type: application/x-protobuf\r\n"), "{head}");
        assert!(head.contains(&format!("user-agent: {USER_AGENT_VALUE}\r\n")), "{head}");
        assert!(head.contains(&format!("authorization: {SECRET}\r\n")), "{head}");
        assert!(head.contains("x-team: blue\r\n"), "{head}");
        assert!(!head.contains("ambient-token"), "{head}");
    }

    #[tokio::test]
    async fn selected_user_agent_and_content_encoding_never_reach_the_wire() {
        let (address, served) = collector(OK).await;
        let mut hostile = destination(&format!("http://{address}/v1/logs"), Duration::from_secs(2));
        hostile.headers = vec![
            ("user-agent".to_owned(), SecretString::from("impostor/9")),
            ("content-encoding".to_owned(), SecretString::from("gzip")),
        ];
        let client = client_for(&hostile).await;

        client
            .send_bytes(post(&format!("http://{address}/v1/logs")))
            .await
            .expect("sent");
        let head = served.await.expect("collector");

        assert!(head.contains(&format!("user-agent: {USER_AGENT_VALUE}\r\n")), "{head}");
        assert!(!head.contains("impostor"), "{head}");
        assert!(!head.contains("content-encoding"), "{head}");
    }

    #[tokio::test]
    async fn response_body_is_never_read() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let writing = Arc::new(AtomicBool::new(false));
        let flag = writing.clone();
        let collector = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_request(&mut stream).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-length: 4000000000\r\n\r\n";
            stream.write_all(head.as_bytes()).await.expect("head");
            let chunk = [b'x'; 16 * 1024];
            // Streams until the client closes the connection; the write then fails.
            while stream.write_all(&chunk).await.is_ok() {
                flag.store(true, Ordering::SeqCst);
            }
        });
        let client = client_for(&destination(
            &format!("http://{address}/v1/logs"),
            Duration::from_secs(30),
        ))
        .await;

        let started = Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            client.send_bytes(post(&format!("http://{address}/v1/logs"))),
        )
        .await
        .expect("send_bytes must return without waiting for the body")
        .expect("sent");

        assert_eq!(response.status(), 200);
        assert!(response.body().is_empty());
        assert!(started.elapsed() < Duration::from_secs(5));
        // The connection is released with the response, which ends the endless stream.
        tokio::time::timeout(Duration::from_secs(5), collector)
            .await
            .expect("dropping the response closes the connection")
            .expect("collector");
        assert!(
            writing.load(Ordering::SeqCst),
            "the collector never got to stream a body"
        );
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_and_is_classified() {
        let (target, followed) = {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("address");
            (address, tokio::spawn(async move { listener.accept().await.is_ok() }))
        };
        let reply: &'static str = Box::leak(
            format!(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://{target}/elsewhere\r\ncontent-length: 0\r\n\r\n"
            )
            .into_boxed_str(),
        );
        let (address, served) = collector(reply).await;
        let client = client_for(&destination(
            &format!("http://{address}/v1/logs"),
            Duration::from_secs(2),
        ))
        .await;

        let response = client
            .send_bytes(post(&format!("http://{address}/v1/logs")))
            .await
            .expect("a redirect is an answer");
        drop(served);

        assert_eq!(response.status(), 307);
        assert_eq!(client.failure_slot().take(), Some(Failure::Redirect));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), followed)
                .await
                .is_err(),
            "the redirect target must never be dialled"
        );
    }

    #[tokio::test]
    async fn an_error_status_is_classified_with_its_code() {
        let (address, _served) = collector("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n").await;
        let client = client_for(&destination(
            &format!("http://{address}/v1/logs"),
            Duration::from_secs(2),
        ))
        .await;

        let response = client
            .send_bytes(post(&format!("http://{address}/v1/logs")))
            .await
            .expect("sent");

        assert_eq!(response.status(), 503);
        assert_eq!(client.failure_slot().take(), Some(Failure::Status(503)));
    }

    // Windows reports a refused loopback connect only after about 2 s, so a shorter deadline turns this into a timeout.
    #[tokio::test]
    async fn a_refused_connection_is_a_connect_failure_that_names_no_url() {
        let address = {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            listener.local_addr().expect("address")
        };
        let client = client_for(&destination(
            &format!("http://{address}/v1/logs"),
            Duration::from_secs(30),
        ))
        .await;

        let error = client
            .send_bytes(post(&format!("http://{address}/v1/logs?k=query-secret")))
            .await
            .expect_err("nothing listens");

        assert_eq!(error.to_string(), "connect");
        assert_eq!(client.failure_slot().take(), Some(Failure::Connect));
    }

    #[tokio::test]
    async fn a_silent_collector_ends_at_the_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let _hold = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("accept");
            std::future::pending::<()>().await;
        });
        let client = client_for(&destination(
            &format!("http://{address}/v1/logs"),
            Duration::from_millis(300),
        ))
        .await;

        let started = Instant::now();
        let error = client
            .send_bytes(post(&format!("http://{address}/v1/logs")))
            .await
            .expect_err("no answer");

        assert_eq!(error.to_string(), "timeout");
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
    }

    /// Builds a client whose lookup never answers, on a runtime that is then dropped: the build must
    /// end in `Failure::Timeout` at the deadline and the runtime must still shut down promptly.
    fn stalled_build_times_out(endpoint: &str, rules: Arc<ProxyRules>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (release, stalled) = stalled_lookup();
        let destination = destination(endpoint, Duration::from_millis(200));

        let started = Instant::now();
        let result = runtime.block_on(Client::build(&destination, &ExtraRoots::default(), rules, stalled));
        let waited = started.elapsed();

        assert!(matches!(result, Err(Failure::Timeout)), "{result:?}");
        assert!(waited < Duration::from_millis(1000), "waited {waited:?}");
        let dropping = Instant::now();
        drop(runtime);
        assert!(
            dropping.elapsed() < Duration::from_millis(500),
            "a runtime that ran a stalled lookup must still drop promptly, took {:?}",
            dropping.elapsed()
        );
        drop(release);
    }

    #[test]
    fn stalled_resolver_never_outlives_the_deadline() {
        stalled_build_times_out("http://collector.test:4318/v1/logs", ProxyRules::direct());
    }

    #[test]
    fn a_stalled_proxy_lookup_never_outlives_the_deadline() {
        for endpoint in [
            "http://collector.test:4318/v1/logs",
            "https://collector.test:4318/v1/logs",
            "http://127.0.0.1:4318/v1/logs",
        ] {
            stalled_build_times_out(endpoint, ProxyRules::proxied_everywhere("http://proxy.invalid:3128"));
        }
    }

    #[tokio::test]
    async fn the_endpoint_name_resolves_through_the_lookup_and_nothing_else() {
        let (address, served) = collector(OK).await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let recorded = asked.clone();
        let destination = destination(
            &format!("http://collector.test:{}/v1/logs", address.port()),
            Duration::from_secs(2),
        );
        let client = Client::build(
            &destination,
            &ExtraRoots::default(),
            ProxyRules::direct(),
            move |name, port| {
                recorded
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((name, port));
                Ok(vec![address])
            },
        )
        .await
        .expect("the client builds");

        let response = client
            .send_bytes(post(&format!("http://collector.test:{}/v1/logs", address.port())))
            .await
            .expect("sent through the pinned address");
        let head = served.await.expect("collector");

        assert_eq!(response.status(), 200);
        assert!(
            head.contains(&format!("host: collector.test:{}", address.port())),
            "{head}"
        );
        assert_eq!(
            *asked.lock().unwrap_or_else(PoisonError::into_inner),
            [("collector.test".to_owned(), address.port())]
        );
    }

    #[tokio::test]
    async fn a_loopback_name_that_resolves_outside_loopback_warns_at_build_time() {
        let outside: SocketAddr = "10.0.0.9:4318".parse().expect("literal");
        let local: SocketAddr = "127.0.0.1:4318".parse().expect("literal");
        for (rules, answer, warns) in [
            (ProxyRules::direct(), outside, true),
            (ProxyRules::direct(), local, false),
            (ProxyRules::proxied_everywhere("http://proxy.invalid:3128"), local, true),
        ] {
            let (capture, _guard) = captured();
            Client::build(
                &destination("http://app.localhost:4318/v1/logs", Duration::from_secs(1)),
                &ExtraRoots::default(),
                rules,
                move |_, _| Ok(vec![answer]),
            )
            .await
            .expect("the client builds");

            let logged = lines(&capture);
            assert_eq!(logged.contains("may leave this host"), warns, "{answer}: {logged}");
            assert!(
                !logged.contains(SECRET) && !logged.contains("app.localhost"),
                "{logged}"
            );
        }
    }

    #[tokio::test]
    async fn a_loopback_literal_warns_at_build_time_only_when_a_proxy_intercepts_it() {
        let proxy: SocketAddr = "10.0.0.2:3128".parse().expect("literal");
        for (rules, warns) in [
            (ProxyRules::direct(), false),
            (ProxyRules::proxied_everywhere("http://proxy.invalid:3128"), true),
        ] {
            let (capture, _guard) = captured();
            Client::build(
                &destination("http://127.0.0.1:4318/v1/logs", Duration::from_secs(1)),
                &ExtraRoots::default(),
                rules,
                move |_, _| Ok(vec![proxy]),
            )
            .await
            .expect("the client builds");

            let logged = lines(&capture);
            assert_eq!(logged.contains("may leave this host"), warns, "{logged}");
            assert!(!logged.contains("non-loopback"), "{logged}");
            assert!(!logged.contains(SECRET) && !logged.contains("127.0.0.1"), "{logged}");
        }
    }

    #[tokio::test]
    async fn a_name_that_resolves_only_to_link_local_addresses_is_refused_and_never_dialled() {
        for answer in [
            "169.254.169.254:4318",
            "[::ffff:169.254.169.254]:4318",
            "[::169.254.169.254]:4318",
            "[64:ff9b::169.254.169.254]:4318",
            "[fe80::1]:4318",
        ] {
            let answer: SocketAddr = answer.parse().expect("literal");
            let result = Client::build(
                &destination("http://collector.test:4318/v1/logs", Duration::from_secs(1)),
                &ExtraRoots::default(),
                ProxyRules::direct(),
                move |_, _| Ok(vec![answer]),
            )
            .await;
            assert!(matches!(result, Err(Failure::Blocked)), "{answer}: {result:?}");
        }
    }

    #[tokio::test]
    async fn only_refused_answers_are_dropped_from_a_mixed_set() {
        let parse = |text: &str| -> SocketAddr { text.parse().expect("literal") };
        let kept = [
            parse("127.0.0.1:4318"),
            parse("10.0.0.5:4318"),
            parse("[::1]:4318"),
            parse("169.253.0.1:4318"),
            parse("100.64.0.1:4318"),
        ];
        let mut mixed = vec![
            parse("169.254.169.254:4318"),
            parse("[::ffff:169.254.169.254]:4318"),
            parse("100.100.100.200:4318"),
            parse("[fd00:ec2::254]:4318"),
        ];
        mixed.extend(kept);
        mixed.push(parse("[fe80::1]:4318"));

        let pins = direct_pins("collector.test", 4318, Duration::from_secs(1), move |_, _| Ok(mixed)).await;

        assert_eq!(pins, Ok(kept.to_vec()));
    }

    #[tokio::test]
    async fn the_pinned_override_reaches_the_dial_for_a_name_the_system_cannot_resolve() {
        let (address, served) = collector(OK).await;
        let destination = destination(
            &format!("http://collector.test:{}/v1/logs", address.port()),
            Duration::from_secs(2),
        );
        let client = Client::build(
            &destination,
            &ExtraRoots::default(),
            ProxyRules::direct(),
            move |_, _| Ok(vec![address]),
        )
        .await
        .expect("the client builds");

        let response = client
            .send_bytes(post(&format!("http://collector.test:{}/v1/logs", address.port())))
            .await
            .expect("sent through the pinned address");
        served.await.expect("collector");

        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn no_host_is_trusted_so_a_resolver_miss_on_the_endpoint_name_fails_closed() {
        use reqwest::dns::Resolve as _;
        let (address, _served) = collector(OK).await;
        // `localhost` resolves to loopback, which the floor refuses unless the name is trusted.
        let client = Client::build(
            &destination(
                &format!("http://localhost:{}/v1/logs", address.port()),
                Duration::from_secs(1),
            ),
            &ExtraRoots::default(),
            ProxyRules::direct(),
            move |_, _| Ok(vec![address]),
        )
        .await
        .expect("the client builds");

        let name: reqwest::dns::Name = "localhost".parse().expect("valid dns name");
        let Err(error) = client.resolver.resolve(name).await else {
            panic!("an unpinned endpoint name must not resolve past the floor");
        };

        assert!(
            matches!(
                error.downcast_ref::<ocx_oci::ssrf::SsrfError>(),
                Some(ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. })
            ),
            "refused by the floor, not by a failed lookup: {error}"
        );
    }

    #[tokio::test]
    async fn a_proxied_route_sends_through_the_proxy_the_rules_chose_with_its_credentials() {
        let (proxy, served) = collector(OK).await;
        let rules = ProxyRules::proxied_everywhere(&format!("http://user:pa55@{proxy}"));
        let client = Client::build(
            &destination("http://collector.test:4318/v1/logs", Duration::from_secs(2)),
            &ExtraRoots::default(),
            rules,
            |_, _| Err(io::Error::other("a proxy named by an IP literal needs no lookup")),
        )
        .await
        .expect("the client builds");

        let response = client
            .send_bytes(post("http://collector.test:4318/v1/logs"))
            .await
            .expect("sent to the proxy");
        let head = served.await.expect("proxy");

        assert_eq!(response.status(), 200);
        assert!(
            head.starts_with("post http://collector.test:4318/v1/logs "),
            "the request must go to the proxy in absolute form: {head}"
        );
        assert!(head.contains("proxy-authorization: basic "), "{head}");
    }

    // reqwest's own proxy read goes through the process environment, which a parallel test must not
    // change, so the case runs in a child process that inherits the proxy variables from its parent.
    #[test]
    #[expect(
        clippy::disallowed_types,
        reason = "the case re-runs this test binary with proxy variables no seam can inject"
    )]
    fn reqwest_never_reads_the_proxy_environment_when_the_rules_say_direct() {
        if ocx_env::__OCX_TESTING_TELEMETRY_PROXY_CHILD.get().is_some() {
            return;
        }
        let decoy = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        decoy.set_nonblocking(true).expect("nonblocking");
        let decoy_url = format!("http://{}", decoy.local_addr().expect("address"));
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .args([
                "--exact",
                "client::tests::child_of_the_proxy_environment_test",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ocx_env::__OCX_TESTING_TELEMETRY_PROXY_CHILD.name, "1")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for variable in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            command.env(variable, &decoy_url);
        }
        let output = command.output().expect("run the child");
        let report = String::from_utf8_lossy(&output.stdout);

        assert!(
            output.status.success(),
            "{report}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(report.contains("1 passed"), "the child case never ran: {report}");
        assert!(
            matches!(decoy.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
            "the client dialled the proxy the environment named"
        );
    }

    #[tokio::test]
    async fn child_of_the_proxy_environment_test() {
        if ocx_env::__OCX_TESTING_TELEMETRY_PROXY_CHILD.get().is_none() {
            return;
        }
        let (address, served) = collector(OK).await;
        let client = client_for(&destination(
            &format!("http://{address}/v1/logs"),
            Duration::from_secs(2),
        ))
        .await;

        let response = client
            .send_bytes(post(&format!("http://{address}/v1/logs")))
            .await
            .expect("sent directly to the collector");
        served.await.expect("the collector got the request");

        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn an_https_endpoint_goes_through_the_proxy_as_a_connect_with_its_credentials() {
        // A refusal ends the tunnel at once, so no TLS handshake is needed to see the request.
        let (proxy, served) = collector("HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n").await;
        let rules = ProxyRules::proxied_everywhere(&format!("http://user:pa55@{proxy}"));
        let client = Client::build(
            &destination("https://collector.test:4318/v1/logs", Duration::from_secs(2)),
            &ExtraRoots::default(),
            rules,
            |_, _| Err(io::Error::other("a proxy named by an IP literal needs no lookup")),
        )
        .await
        .expect("the client builds");

        let error = client
            .send_bytes(post("https://collector.test:4318/v1/logs"))
            .await
            .expect_err("the proxy refused the tunnel");
        let head = served.await.expect("proxy");

        assert_eq!(error.to_string(), "connect");
        assert!(head.starts_with("connect collector.test:4318 "), "{head}");
        assert!(head.contains("proxy-authorization: basic "), "{head}");
    }

    #[tokio::test]
    async fn the_proxy_rules_read_in_new_is_bounded_by_the_deadline() {
        let (release, hold) = std::sync::mpsc::channel::<()>();
        // Unblocks on its own after 3 s, so a read that is not bounded fails the assertion instead of hanging.
        let stalled = move || {
            let _ = hold.recv_timeout(Duration::from_secs(3));
            ProxyRules::direct()
        };
        let started = Instant::now();

        let result = Client::new_with(
            &destination("http://127.0.0.1:4318/v1/logs", Duration::from_millis(200)),
            &ExtraRoots::default(),
            stalled,
        )
        .await;

        assert!(matches!(result, Err(Failure::Timeout)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
        drop(release);
    }

    #[tokio::test]
    async fn a_lookup_failure_or_an_empty_answer_is_a_connect_failure() {
        let destination = destination("http://collector.test:4318/v1/logs", Duration::from_secs(1));
        for answer in [Err(io::Error::other("no such host")), Ok(Vec::new())] {
            let result = Client::build(
                &destination,
                &ExtraRoots::default(),
                ProxyRules::direct(),
                move |_, _| answer,
            )
            .await;
            assert!(matches!(result, Err(Failure::Connect)), "{result:?}");
        }
    }

    #[tokio::test]
    async fn an_ip_literal_skips_the_endpoint_lookup() {
        let asked = Arc::new(AtomicBool::new(false));
        for endpoint in ["http://[::1]:4318/v1/logs", "http://127.0.0.1:4318/v1/logs"] {
            let flag = asked.clone();
            let built = Client::build(
                &destination(endpoint, Duration::from_secs(1)),
                &ExtraRoots::default(),
                ProxyRules::direct(),
                move |_, _| {
                    flag.store(true, Ordering::SeqCst);
                    Err(io::Error::other("must not be asked"))
                },
            )
            .await;
            assert!(built.is_ok(), "{endpoint}: {built:?}");
        }
        assert!(!asked.load(Ordering::SeqCst), "a lookup ran where none was owed");
    }

    #[tokio::test]
    async fn a_proxied_route_looks_up_the_proxy_the_scheme_picks_and_never_the_endpoint() {
        let proxy: SocketAddr = "10.0.0.2:3128".parse().expect("literal");
        for (endpoint, rules, expected) in [
            (
                "http://collector.test:4318/v1/logs",
                ProxyRules::proxied_everywhere("http://proxy.invalid:3128"),
                Some(("proxy.invalid", 3128)),
            ),
            (
                "https://collector.test:4318/v1/logs",
                ProxyRules::proxied_everywhere("http://proxy.invalid"),
                Some(("proxy.invalid", 80)),
            ),
            // An IP literal endpoint is still dialled through the proxy, so the proxy still resolves.
            (
                "http://127.0.0.1:4318/v1/logs",
                ProxyRules::proxied_everywhere("http://proxy.invalid:3128"),
                Some(("proxy.invalid", 3128)),
            ),
            // A proxy named by an IP literal needs no lookup.
            (
                "http://collector.test:4318/v1/logs",
                ProxyRules::proxied_everywhere("http://10.0.0.2:3128"),
                None,
            ),
        ] {
            let asked = Arc::new(Mutex::new(Vec::new()));
            let recorded = asked.clone();
            let built = Client::build(
                &destination(endpoint, Duration::from_secs(1)),
                &ExtraRoots::default(),
                rules,
                move |name, port| {
                    recorded
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((name, port));
                    Ok(vec![proxy])
                },
            )
            .await;
            assert!(built.is_ok(), "{endpoint}: {built:?}");
            let asked = asked.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let expected: Vec<(String, u16)> = expected
                .into_iter()
                .map(|(name, port)| (name.to_owned(), port))
                .collect();
            assert_eq!(asked, expected, "{endpoint}");
        }
    }

    // The resolver answers the pinned proxy name whatever case it is asked in: the name cannot be
    // resolved (`.invalid`), so only the pin can answer it. Nothing here sends a request.
    #[tokio::test]
    async fn a_mixed_case_proxy_host_takes_the_pinned_answer_with_no_lookup() {
        use reqwest::dns::Resolve as _;
        let answer: SocketAddr = "10.0.0.2:3128".parse().expect("literal");
        let asked = Arc::new(Mutex::new(Vec::new()));
        let recorded = asked.clone();
        let rules = ProxyRules::proxied_everywhere("http://Proxy.Example.invalid:3128");
        let resolver = GuardedResolver::new(Arc::new(Vec::new()), rules.clone());

        let resolver = pin_proxy(
            resolver,
            &rules,
            (DialScheme::Http, "collector.test", 4318),
            Duration::from_secs(1),
            move |name, port| {
                recorded
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((name, port));
                Ok(vec![answer])
            },
        )
        .await
        .expect("the proxy resolves");

        // Hyper asks with the spelling of the proxy URL, not the lower-cased name looked up.
        let name: reqwest::dns::Name = "Proxy.Example.invalid".parse().expect("valid dns name");
        let addresses: Vec<SocketAddr> = resolver
            .resolve(name)
            .await
            .expect("the pinned answer, not a lookup of an unresolvable name")
            .collect();
        assert_eq!(addresses, [answer]);
        assert_eq!(
            *asked.lock().unwrap_or_else(PoisonError::into_inner),
            [("proxy.example.invalid".to_owned(), 3128)],
            "the proxy is looked up once, by the client, under the deadline"
        );
    }

    #[test]
    fn failure_classes_print_as_one_word_each() {
        for (failure, word) in [
            (Failure::Connect, "connect"),
            (Failure::Blocked, "blocked"),
            (Failure::Timeout, "timeout"),
            (Failure::Status(503), "status 503"),
            (Failure::Redirect, "redirect"),
            (Failure::Encode, "encode"),
            (Failure::Panic, "panic"),
            (Failure::Thread, "thread"),
        ] {
            assert_eq!(failure.to_string(), word);
        }
    }
}
