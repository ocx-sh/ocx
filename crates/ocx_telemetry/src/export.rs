// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one export: records collected in memory and sent as a single request.
//!
//! The records are minted straight into SDK types and handed to `opentelemetry-otlp`'s exporter
//! once, as one batch, over [`Client`]. The crate's processors, its worker threads and its default
//! transport are never used: the exemption is argued in `.claude/artifacts/adr_otel_telemetry.md`.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;

use ocx_util::tls::ExtraRoots;
use opentelemetry_http::HttpClient;
use opentelemetry_otlp::{LogExporter, Protocol, RetryPolicy, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::{LogBatch, LogExporter as _};

use crate::client::{Client, Failure};
use crate::destination::Destination;
use crate::detached;
use crate::gate::Decision;
use crate::identity;
use crate::payload::{self, CommandOutcome, PackageRecordInput, WireRecords};
use crate::resource::ResourceInputs;

/// What the CLI keeps from the start of a run to its end.
pub struct Slot {
    decision: Decision,
    resource: Box<dyn FnOnce() -> ResourceInputs + Send>,
    installation_id_file: PathBuf,
    extra_roots: ExtraRoots,
}

// The resource producer is opaque, and the decision prints its destination without the query.
impl fmt::Debug for Slot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Slot")
            .field("decision", &self.decision)
            .field("installation_id_file", &self.installation_id_file)
            .field("extra_roots", &self.extra_roots)
            .finish_non_exhaustive()
    }
}

impl Slot {
    /// A slot for a decided run.
    ///
    /// `resource` probes the process (host name, CI variables, `OTEL_RESOURCE_ATTRIBUTES`) and is
    /// called once, by [`Slot::finish`], and only for an armed run with a destination: an unarmed
    /// run never probes. It runs on its own thread inside the destination's deadline, so a probe
    /// that stalls or panics costs at most the timeout. `installation_id_file` is read or created
    /// under the same condition, and `extra_roots` are the CA roots the client trusts beyond the
    /// embedded set.
    pub fn new(
        decision: Decision,
        resource: impl FnOnce() -> ResourceInputs + Send + 'static,
        installation_id_file: PathBuf,
        extra_roots: ExtraRoots,
    ) -> Self {
        Self {
            decision,
            resource: Box::new(resource),
            installation_id_file,
            extra_roots,
        }
    }

    /// Sends the command and package records as one request, within the destination's
    /// deadline. Does nothing for an unarmed run or one without a destination, and never
    /// changes the outcome: every failure ends in at most one debug line.
    pub async fn finish(self, outcome: CommandOutcome, packages: Vec<PackageRecordInput>) {
        let transport = Otlp {
            extra_roots: self.extra_roots.clone(),
        };
        self.finish_with(outcome, packages, transport).await;
    }

    async fn finish_with<T: Transport>(self, outcome: CommandOutcome, packages: Vec<PackageRecordInput>, transport: T) {
        let destination = match self.decision {
            Decision::Off { reason, .. } => {
                tracing::debug!(target: "ocx_telemetry", "telemetry: off ({})", reason.label());
                return;
            }
            Decision::Armed {
                destination, armed_by, ..
            } => {
                tracing::debug!(target: "ocx_telemetry", "telemetry: armed by {}", armed_by.label());
                let Some(destination) = destination else {
                    tracing::debug!(target: "ocx_telemetry", "telemetry: no destination");
                    return;
                };
                destination
            }
        };
        tracing::debug!(target: "ocx_telemetry", "{}", destination.audit_line());

        let timeout = destination.timeout;
        let probe = self.resource;
        let id_file = self.installation_id_file;
        let mut task = tokio::spawn(async move {
            // A probe that panics is a failure class, never a panic out of `finish`.
            let resource = detached::run("ocx-telemetry-resource", probe).await?;
            let installation_id = identity::installation_id(&id_file).await;
            // A 10,000-package batch takes tens of milliseconds to build and encode, which would
            // stall a runtime worker; the audit line is logged back here, off the encode thread.
            let (audit_line, wire) = detached::run("ocx-telemetry-encode", move || {
                let payload = payload::build(&resource, installation_id.as_deref(), &outcome, &packages);
                (payload.audit_line(), payload.to_wire())
            })
            .await?;
            tracing::debug!(target: "ocx_telemetry", "{audit_line}");
            transport.send(&destination, &wire).await
        });

        // One deadline for the probe, the id, the build and the request together.
        let failure = match tokio::time::timeout(timeout, &mut task).await {
            Ok(Ok(Ok(()))) => return,
            Ok(Ok(Err(failure))) => failure,
            // A `JoinError` names the panic payload; only the class is logged.
            Ok(Err(_)) => Failure::Panic,
            Err(_) => {
                // Aborted, not awaited: the point of the deadline is not to wait for the task.
                task.abort();
                Failure::Timeout
            }
        };
        tracing::debug!(target: "ocx_telemetry", "telemetry: export failed: {failure}");
    }
}

/// How one run's records reach a collector. The seam the tests count exports through.
trait Transport: Send + 'static {
    fn send<'a>(
        &'a self,
        destination: &'a Destination,
        wire: &'a WireRecords,
    ) -> impl Future<Output = Result<(), Failure>> + Send + 'a;
}

/// The production transport: ocx's own client, resolving through the environment's proxy rules.
struct Otlp {
    extra_roots: ExtraRoots,
}

impl Transport for Otlp {
    async fn send(&self, destination: &Destination, wire: &WireRecords) -> Result<(), Failure> {
        let client = Client::new(destination, &self.extra_roots).await?;
        export_through(client, destination, wire).await
    }
}

/// Exactly one `export` of one batch through `client`.
///
/// The failure class comes from the client's slot: the exporter flattens every error into a string
/// that may embed the endpoint, so what it returned is never read.
async fn export_through(client: Client, destination: &Destination, wire: &WireRecords) -> Result<(), Failure> {
    let failure = client.failure_slot();
    let exporter = build_exporter(destination, client)?;
    let exported = export_batch(exporter, wire).await;
    match (failure.take(), exported) {
        (Some(failure), _) => Err(failure),
        (None, Err(_)) => Err(Failure::Connect),
        (None, Ok(())) => Ok(()),
    }
}

/// The exporter configured for one request to `destination` through `client`.
fn build_exporter(destination: &Destination, client: impl HttpClient + 'static) -> Result<LogExporter, Failure> {
    LogExporter::builder()
        .with_http()
        .with_endpoint(destination.endpoint.as_str())
        .with_protocol(Protocol::HttpBinary)
        .with_timeout(destination.timeout)
        // The OTLP spec's SHOULD-retry is deliberately not followed: the deadline is shorter than any useful wait.
        .with_retry_policy(RetryPolicy::disabled())
        .with_http_client(client)
        .build()
        .map_err(|_| Failure::Encode)
}

async fn export_batch(mut exporter: LogExporter, wire: &WireRecords) -> opentelemetry_sdk::error::OTelSdkResult {
    exporter.set_resource(wire.resource());
    exporter.export(LogBatch::new(&wire.batch_data())).await
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use ocx_oci::ssrf::ProxyRules;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::gate::{ArmedBy, OffReason};
    use crate::log_capture::{captured, lines};
    use crate::payload::{CommandResult, PackageAction, PackageRole};
    use crate::testing::{SECRET_HEADER, destination};

    const SECRET_QUERY: &str = "query-secret-9f3";

    /// Counts sends and the records each carried; optionally panics instead of sending.
    #[derive(Clone, Default)]
    struct Counting {
        batches: Arc<Mutex<Vec<usize>>>,
        panics: bool,
    }

    impl Counting {
        fn calls(&self) -> usize {
            self.batches.lock().expect("batches").len()
        }
    }

    impl Transport for Counting {
        async fn send(&self, _destination: &Destination, wire: &WireRecords) -> Result<(), Failure> {
            assert!(!self.panics, "a transport that breaks");
            self.batches.lock().expect("batches").push(wire.batch_data().len());
            Ok(())
        }
    }

    /// The production path with the proxy routing injected, so a local listener is dialled directly.
    struct Direct;

    impl Transport for Direct {
        async fn send(&self, destination: &Destination, wire: &WireRecords) -> Result<(), Failure> {
            let client = Client::build(destination, &ExtraRoots::default(), ProxyRules::direct(), |_, _| {
                Err(io::Error::other("an IP literal needs no lookup"))
            })
            .await?;
            export_through(client, destination, wire).await
        }
    }

    /// A transport that never resolves, the way a collector that accepts and never answers does when
    /// the client's own timeout is out of the picture.
    struct Never;

    impl Transport for Never {
        async fn send(&self, _destination: &Destination, _wire: &WireRecords) -> Result<(), Failure> {
            std::future::pending().await
        }
    }

    /// Runs `finish_with` on its own thread and runtime, so a probe that blocks its caller fails the
    /// test at `limit` instead of hanging the run. Returns how long it took and the debug lines.
    fn finish_on_own_thread(slot: Slot, transport: impl Transport, limit: Duration) -> (Duration, String) {
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let (capture, _guard) = captured();
            let started = Instant::now();
            runtime.block_on(slot.finish_with(outcome(), Vec::new(), transport));
            let _ = done.send((started.elapsed(), lines(&capture)));
        });
        result.recv_timeout(limit).expect("finish did not return")
    }

    fn slot(decision: Decision, id_file: PathBuf) -> Slot {
        Slot::new(decision, ResourceInputs::default, id_file, ExtraRoots::default())
    }

    /// A slot whose resource producer counts its calls.
    fn probing_slot(decision: Decision, id_file: PathBuf) -> (Slot, Arc<AtomicUsize>) {
        let probes = Arc::new(AtomicUsize::new(0));
        let counted = probes.clone();
        let slot = Slot::new(
            decision,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                ResourceInputs::default()
            },
            id_file,
            ExtraRoots::default(),
        );
        (slot, probes)
    }

    fn armed(destination: Option<Destination>) -> Decision {
        Decision::Armed {
            destination,
            armed_by: ArmedBy::Switch,
            warnings: Vec::new(),
        }
    }

    fn outcome() -> CommandOutcome {
        CommandOutcome {
            name: "package install".to_owned(),
            duration: Duration::from_millis(40),
            exit_code: 0,
            result: CommandResult::Succeeded,
        }
    }

    fn package(purl: &str) -> PackageRecordInput {
        PackageRecordInput {
            purl: purl.to_owned(),
            action: PackageAction::Installed,
            role: PackageRole::Root,
            store_hit: false,
            download_size: Some(10),
            resolve: None,
            download: None,
            extract: None,
        }
    }

    /// Answers every request on `listener` with `status`, counting them; `None` accepts and never answers.
    fn serve(listener: TcpListener, status: Option<u16>) -> Arc<AtomicUsize> {
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = requests.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((mut stream, _)) = listener.accept().await {
                let Some(status) = status else {
                    held.push(stream);
                    continue;
                };
                read_request(&mut stream).await;
                counted.fetch_add(1, Ordering::SeqCst);
                let answer = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                let _ = stream.write_all(answer.as_bytes()).await;
            }
        });
        requests
    }

    async fn read_request(stream: &mut TcpStream) {
        let mut received = Vec::new();
        let mut chunk = [0_u8; 4096];
        let mut needed = None;
        loop {
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            received.extend_from_slice(&chunk[..read]);
            if needed.is_none()
                && let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n")
            {
                let head = String::from_utf8_lossy(&received[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: ")?.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                needed = Some(end + 4 + length);
            }
            if needed.is_some_and(|total| received.len() >= total) {
                return;
            }
        }
    }

    async fn listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = format!(
            "http://127.0.0.1:{}/v1/logs?api-key={SECRET_QUERY}",
            listener.local_addr().expect("address").port()
        );
        (listener, endpoint)
    }

    #[tokio::test]
    async fn off_never_reaches_the_exporter() {
        let reasons = [
            OffReason::DoNotTrack,
            OffReason::SdkDisabled,
            OffReason::LogsExporterNone,
            OffReason::Offline,
            OffReason::Switch,
            OffReason::Unusable,
            OffReason::Locked,
            OffReason::NotEnabled,
        ];
        let mut decisions: Vec<Decision> = reasons
            .into_iter()
            .map(|reason| Decision::Off {
                reason,
                warnings: Vec::new(),
            })
            .collect();
        decisions.push(armed(None));
        for decision in decisions {
            let home = tempfile::tempdir().expect("tempdir");
            let id_file = home.path().join("state").join("installation-id");
            let double = Counting::default();

            let (slot, probes) = probing_slot(decision, id_file.clone());
            slot.finish_with(outcome(), vec![package("pkg:oci/a")], double.clone())
                .await;

            assert_eq!(double.calls(), 0);
            assert_eq!(
                probes.load(Ordering::SeqCst),
                0,
                "an unarmed run must not probe the process"
            );
            assert_eq!(
                std::fs::read_dir(home.path()).expect("read dir").count(),
                0,
                "an unarmed run or one without a destination writes nothing"
            );
        }
    }

    #[test]
    fn decision_and_slot_debug_never_print_the_query() {
        let decision = armed(Some(destination(
            &format!("http://127.0.0.1:4318/v1/logs?api-key={SECRET_QUERY}#frag"),
            Duration::from_secs(1),
        )));
        let slot = slot(
            armed(Some(destination(
                &format!("http://127.0.0.1:4318/v1/logs?api-key={SECRET_QUERY}"),
                Duration::from_secs(1),
            ))),
            PathBuf::from("id"),
        );
        for printed in [format!("{decision:?}"), format!("{slot:?}")] {
            assert!(printed.contains("127.0.0.1:4318/v1/logs"), "{printed}");
            for needle in [SECRET_QUERY, "api-key", "frag", SECRET_HEADER] {
                assert!(!printed.contains(needle), "{needle} leaked into {printed}");
            }
        }
    }

    #[tokio::test]
    async fn exactly_one_export_per_finish() {
        let home = tempfile::tempdir().expect("tempdir");
        let double = Counting::default();
        let packages = vec![package("pkg:oci/a"), package("pkg:oci/b"), package("pkg:oci/c")];
        let decision = armed(Some(destination("http://127.0.0.1:1/v1/logs", Duration::from_secs(5))));

        let (slot, probes) = probing_slot(decision, home.path().join("installation-id"));
        slot.finish_with(outcome(), packages, double.clone()).await;
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "the resource is probed once, for the one export"
        );

        assert_eq!(
            *double.batches.lock().expect("batches"),
            vec![4],
            "one batch, command + 3 packages"
        );
        assert!(home.path().join("installation-id").is_file());
    }

    #[tokio::test]
    async fn a_panicking_export_is_one_panic_line() {
        let home = tempfile::tempdir().expect("tempdir");
        let (capture, _guard) = captured();
        let decision = armed(Some(destination("http://127.0.0.1:1/v1/logs", Duration::from_secs(5))));

        slot(decision, home.path().join("id"))
            .finish_with(
                outcome(),
                Vec::new(),
                Counting {
                    panics: true,
                    ..Counting::default()
                },
            )
            .await;

        let logged = lines(&capture);
        assert_eq!(logged.matches("export failed").count(), 1, "{logged}");
        assert!(logged.contains("export failed: panic"), "{logged}");
    }

    #[tokio::test]
    async fn a_transport_that_never_resolves_ends_at_the_outer_deadline() {
        let home = tempfile::tempdir().expect("tempdir");
        let (capture, _guard) = captured();
        let timeout = Duration::from_millis(300);
        let decision = armed(Some(destination("http://127.0.0.1:1/v1/logs", timeout)));

        let started = Instant::now();
        tokio::time::timeout(
            Duration::from_secs(5),
            slot(decision, home.path().join("id")).finish_with(outcome(), Vec::new(), Never),
        )
        .await
        .expect("finish must end at its own deadline");

        let took = started.elapsed();
        assert!(took >= timeout, "finish returned before the deadline: {took:?}");
        assert!(took < timeout + Duration::from_millis(1500), "finish took {took:?}");
        let logged = lines(&capture);
        assert_eq!(logged.matches("export failed").count(), 1, "{logged}");
        assert!(logged.contains("export failed: timeout"), "{logged}");
    }

    #[tokio::test]
    async fn a_collector_that_never_answers_ends_in_the_client_timeout() {
        let home = tempfile::tempdir().expect("tempdir");
        let (listener, endpoint) = listener().await;
        serve(listener, None);
        let (capture, _guard) = captured();
        let timeout = Duration::from_millis(400);
        let decision = armed(Some(destination(&endpoint, timeout)));

        let started = Instant::now();
        slot(decision, home.path().join("id"))
            .finish_with(outcome(), Vec::new(), Direct)
            .await;

        let took = started.elapsed();
        assert!(took < timeout + Duration::from_millis(1500), "finish took {took:?}");
        let logged = lines(&capture);
        assert!(logged.contains("export failed: timeout"), "{logged}");
    }

    #[test]
    fn a_resource_probe_that_never_returns_costs_at_most_the_timeout() {
        let home = tempfile::tempdir().expect("tempdir");
        let timeout = Duration::from_millis(300);
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let slot = Slot::new(
            armed(Some(destination("http://127.0.0.1:1/v1/logs", timeout))),
            move || {
                let _ = hold.recv();
                ResourceInputs::default()
            },
            home.path().join("id"),
            ExtraRoots::default(),
        );
        let double = Counting::default();

        let (took, logged) = finish_on_own_thread(slot, double.clone(), Duration::from_secs(5));

        assert!(took >= timeout, "finish returned before the deadline: {took:?}");
        assert!(took < timeout + Duration::from_millis(1500), "finish took {took:?}");
        assert!(logged.contains("export failed: timeout"), "{logged}");
        assert_eq!(double.calls(), 0, "nothing is sent without a resource");
        drop(release);
    }

    #[tokio::test]
    async fn a_resource_probe_that_panics_is_one_panic_line_from_its_own_thread() {
        let home = tempfile::tempdir().expect("tempdir");
        let (capture, _guard) = captured();
        let ran_on = Arc::new(Mutex::new(None));
        let recorded = ran_on.clone();
        let slot = Slot::new(
            armed(Some(destination("http://127.0.0.1:1/v1/logs", Duration::from_secs(5)))),
            move || {
                *recorded.lock().expect("thread name") = std::thread::current().name().map(str::to_owned);
                panic!("a host name lookup that breaks")
            },
            home.path().join("id"),
            ExtraRoots::default(),
        );
        let double = Counting::default();

        slot.finish_with(outcome(), Vec::new(), double.clone()).await;

        let logged = lines(&capture);
        assert_eq!(logged.matches("export failed").count(), 1, "{logged}");
        assert!(logged.contains("export failed: panic"), "{logged}");
        assert_eq!(double.calls(), 0);
        // Only the detached thread carries this name; a probe run inline on the task would not.
        assert_eq!(
            ran_on.lock().expect("thread name").as_deref(),
            Some("ocx-telemetry-resource")
        );
    }

    /// Keeps the body of every request the exporter sends and answers each with a 200.
    #[derive(Clone, Debug, Default)]
    struct Recording(Arc<Mutex<Vec<opentelemetry_http::Bytes>>>);

    #[async_trait::async_trait]
    impl HttpClient for Recording {
        async fn send_bytes(
            &self,
            request: opentelemetry_http::Request<opentelemetry_http::Bytes>,
        ) -> Result<opentelemetry_http::Response<opentelemetry_http::Bytes>, opentelemetry_http::HttpError> {
            self.0.lock().expect("bodies").push(request.into_body());
            Ok(opentelemetry_http::Response::new(opentelemetry_http::Bytes::new()))
        }
    }

    #[tokio::test]
    async fn ten_thousand_package_records_encode_as_one_request() {
        let packages: Vec<PackageRecordInput> = (0..10_000)
            .map(|index| PackageRecordInput {
                purl: format!("pkg:oci/package-{index}?repository_url=ocx.sh/tools&arch=amd64"),
                ..package("pkg:oci/unused")
            })
            .collect();
        let wire = payload::build(&ResourceInputs::default(), None, &outcome(), &packages).to_wire();
        let recording = Recording::default();
        let exporter = build_exporter(
            &destination("http://127.0.0.1:1/v1/logs", Duration::from_secs(5)),
            recording.clone(),
        )
        .expect("the exporter builds");

        export_batch(exporter, &wire).await.expect("the batch is exported");

        let bodies = recording.0.lock().expect("bodies");
        assert_eq!(bodies.len(), 1, "every record rides one request");
        // Each package record carries its purl once in the protobuf body, so the count after
        // encoding is the number of package records the request holds.
        let needle = b"pkg:oci/package-";
        let encoded = bodies[0]
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count();
        assert_eq!(encoded, 10_000);
    }

    #[tokio::test]
    async fn export_failure_logs_only_its_class() {
        let home = tempfile::tempdir().expect("tempdir");
        let (closed, endpoint) = listener().await;
        drop(closed);
        // Windows reports a refused loopback connect only after about 2 s, so a shorter deadline turns this into a timeout.
        let (capture, _guard) = captured();
        let decision = armed(Some(destination(&endpoint, Duration::from_secs(30))));

        slot(decision, home.path().join("id"))
            .finish_with(outcome(), Vec::new(), Direct)
            .await;

        let logged = lines(&capture);
        assert!(logged.contains("export failed: connect"), "{logged}");
        for forbidden in [SECRET_QUERY, "api-key", SECRET_HEADER, "reqwest"] {
            assert!(!logged.contains(forbidden), "{forbidden:?} leaked into: {logged}");
        }
    }

    #[tokio::test]
    async fn a_failing_status_is_one_request_and_one_line() {
        let home = tempfile::tempdir().expect("tempdir");
        let (listener, endpoint) = listener().await;
        let requests = serve(listener, Some(503));
        let (capture, _guard) = captured();
        let decision = armed(Some(destination(&endpoint, Duration::from_secs(5))));

        slot(decision, home.path().join("id"))
            .finish_with(outcome(), vec![package("pkg:oci/a")], Direct)
            .await;

        assert_eq!(requests.load(Ordering::SeqCst), 1, "no retry");
        let logged = lines(&capture);
        assert_eq!(logged.matches("export failed").count(), 1, "{logged}");
        assert!(logged.contains("export failed: status 503"), "{logged}");
    }

    #[tokio::test]
    async fn a_healthy_collector_gets_one_request_and_no_failure_line() {
        let home = tempfile::tempdir().expect("tempdir");
        let (listener, endpoint) = listener().await;
        let requests = serve(listener, Some(200));
        let (capture, _guard) = captured();
        let decision = armed(Some(destination(&endpoint, Duration::from_secs(5))));

        slot(decision, home.path().join("id"))
            .finish_with(outcome(), vec![package("pkg:oci/a")], Direct)
            .await;

        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let logged = lines(&capture);
        assert!(!logged.contains("export failed"), "{logged}");
        assert!(logged.contains("telemetry: armed by OCX_TELEMETRY"), "{logged}");
        assert!(logged.contains("telemetry: destination http://127.0.0.1:"), "{logged}");
        assert!(logged.contains("telemetry payload {"), "{logged}");
        assert!(!logged.contains(SECRET_QUERY), "{logged}");
    }

    /// The source text of this crate with every comment line removed.
    fn source_without_comments() -> String {
        let mut text = String::new();
        let mut read = 0;
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).expect("read src") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    read += 1;
                    for line in std::fs::read_to_string(&path).expect("read source").lines() {
                        // Comment-only lines and whole-line doc comments: a needle named in prose is not code.
                        if !line.trim_start().starts_with("//") {
                            text.push_str(line);
                            text.push('\n');
                        }
                    }
                }
            }
        }
        assert!(read >= 8, "the reader found {read} source files");
        text
    }

    #[test]
    fn no_crate_processor_or_transport_is_named() {
        let code = source_without_comments();
        // Split literals: the scan reads this module too, and must not match its own needles.
        let forbidden = [
            concat!("Batch", "LogProcessor"),
            concat!("Simple", "LogProcessor"),
            concat!("with_batch", "_exporter"),
            concat!("with_simple", "_exporter"),
            concat!("reqwest", "-client"),
        ];
        for needle in forbidden {
            assert!(!code.contains(needle), "{needle} is named in the crate's source");
        }
        let needed = [
            concat!("LogBatch", "::new("),
            concat!(".with_http", "_client("),
            concat!("RetryPolicy", "::disabled()"),
        ];
        for needed in needed {
            assert!(code.contains(needed), "the export no longer names {needed}");
        }
    }
}
