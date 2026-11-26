// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The seam through which a source's signed catalog is checked.
//!
//! The index tier names no trust type: the application layer supplies the verifier and this
//! tier acts on its verdict.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ocx_config::IndexSignatureMode;
use serde::{Deserialize, Serialize};

use crate::error::{ArcError, Error};
use crate::trust_error::IndexTrustError;
use crate::wire::CatalogDocument;

/// What a [`CatalogVerifier`] concluded about one catalog and its signature.
#[derive(Debug, Clone)]
pub enum CatalogVerdict {
    /// The signature, signer and expiry all hold; `document` is the parsed catalog.
    Verified {
        document: CatalogDocument,
        /// A display description of the matched signer, never certificate contents.
        signer: String,
    },
    /// The source serves no signature.
    Unsigned,
    /// A signature is present and does not hold; `reason` is for a human.
    Invalid(String),
    /// The signature is genuine and its signer is not on the trusted list.
    PublisherMismatch,
    /// The signature holds and its validity window has ended, or it names no end.
    Expired { expires: Option<DateTime<Utc>> },
    /// The signer list holds no entry this build understands.
    Unrecognized,
}

/// Checks catalog bytes against the signature served beside them.
#[async_trait]
pub trait CatalogVerifier: Send + Sync {
    /// Judges `catalog_bytes` as served, with `signature` the detached signature bytes when the source served one.
    async fn check(&self, catalog_bytes: &[u8], signature: Option<&[u8]>) -> CatalogVerdict;
}

// ── The per-source check ─────────────────────────────────────────────────────

/// Why a catalog or one of its roots is not trusted, before the mode decides what happens.
#[derive(Debug, Clone)]
pub(crate) enum TrustFailure {
    Unsigned,
    /// A read of the catalog or its signature failed under `warn`; `enforce` propagates the transport error instead.
    Unfetched {
        what: &'static str,
        /// The read's own error, so a strict reader of a catalog it could not read exits as that error does.
        error: ArcError,
    },
    Invalid(String),
    PublisherMismatch,
    Expired(Option<DateTime<Utc>>),
    Unrecognized,
    Rollback {
        served: DateTime<Utc>,
        recorded: DateTime<Utc>,
    },
    RootDigestMismatch {
        package: String,
    },
    PackageNotInCatalog {
        package: String,
    },
    RootNotFound {
        package: String,
    },
}

impl TrustFailure {
    /// Whether one uncached re-read can plausibly turn this verdict around: a deploy that was
    /// mid-flight serves a catalog, its signature or a root from two generations.
    pub(crate) fn refetchable(&self) -> bool {
        !matches!(
            self,
            Self::Unsigned | Self::Unfetched { .. } | Self::Expired(_) | Self::Unrecognized
        )
    }

    /// The reason a `warn` line gives: the refusal text, except for a read that failed in transit.
    fn reason(&self, registry: &str) -> String {
        match self {
            Self::Unfetched { what, error } => format!(
                "the {what} could not be fetched: {}",
                error.as_error().short_transport_cause()
            ),
            other => other.clone().into_error(registry).to_string(),
        }
    }

    fn into_error(self, registry: &str) -> IndexTrustError {
        let registry = registry.to_string();
        match self {
            // Only `warn` records an unfetched read, and `warn` words it with `reason`, never this error.
            Self::Unsigned | Self::Unfetched { .. } => IndexTrustError::SignatureNotFound { registry },
            Self::Invalid(reason) => IndexTrustError::SignatureInvalid { registry, reason },
            Self::PublisherMismatch => IndexTrustError::PublisherMismatch { registry },
            Self::Expired(expires) => IndexTrustError::CatalogExpired { registry, expires },
            Self::Unrecognized => IndexTrustError::PublishersUnrecognized { registry },
            Self::Rollback { served, recorded } => IndexTrustError::CatalogRollback {
                registry,
                served,
                recorded,
            },
            Self::RootDigestMismatch { package } => IndexTrustError::RootDigestMismatch { registry, package },
            Self::PackageNotInCatalog { package } => IndexTrustError::PackageNotInCatalog { registry, package },
            Self::RootNotFound { package } => IndexTrustError::RootNotFound { registry, package },
        }
    }
}

/// One fetched generation of a source's catalog and its verdict.
#[derive(Debug)]
pub(crate) struct CatalogState {
    /// The catalog exactly as served; `None` when the source served none or could not be reached.
    pub(crate) bytes: Option<Vec<u8>>,
    /// The parsed catalog, held only when the verdict was `Verified`: nothing else may be compared against.
    document: Option<CatalogDocument>,
    pub(crate) failure: Option<TrustFailure>,
}

impl CatalogState {
    /// A state with no catalog to judge: the source served none, or was unreachable under `warn`.
    pub(crate) fn unavailable() -> Self {
        Self {
            bytes: None,
            document: None,
            failure: Some(TrustFailure::Unsigned),
        }
    }

    /// A state for a read that failed in transit under `warn`: `bytes` is the catalog when only its signature was lost.
    pub(crate) fn unfetched(bytes: Option<Vec<u8>>, what: &'static str, error: Error) -> Self {
        Self {
            bytes,
            document: None,
            failure: Some(TrustFailure::Unfetched {
                what,
                error: ArcError::from(error),
            }),
        }
    }

    /// A state for a catalog whose signature the transport refused as oversize.
    pub(crate) fn invalid(bytes: Vec<u8>, reason: String) -> Self {
        Self {
            bytes: Some(bytes),
            document: None,
            failure: Some(TrustFailure::Invalid(reason)),
        }
    }

    /// A state for a catalog with no signature to judge it by.
    pub(crate) fn unsigned(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Some(bytes),
            document: None,
            failure: Some(TrustFailure::Unsigned),
        }
    }
}

#[derive(Default)]
pub(crate) struct Slot {
    pub(crate) snapshot: Option<Arc<CatalogState>>,
    /// Whether the one uncached re-read of this source has been spent.
    pub(crate) refetched: bool,
}

/// The catalog check one [`OcxIndex`](super::OcxIndex) carries: the verifier, the mode and the held verdict.
pub(crate) struct CatalogCheck {
    verifier: Arc<dyn CatalogVerifier>,
    pub(crate) mode: IndexSignatureMode,
    /// Held across the fetch so concurrent callers share one, and replaced at most once.
    pub(crate) slot: tokio::sync::Mutex<Slot>,
    warned: AtomicBool,
    warnings: AtomicUsize,
    high_water: Option<PathBuf>,
    /// A record problem found while a quiet view held the fetch, kept for the first caller that may speak.
    record_problem: std::sync::Mutex<Option<String>>,
    record_warned: AtomicBool,
}

/// A record over this is garbage: it holds one timestamp.
const HIGH_WATER_CAP: u64 = 64 * 1024;

impl CatalogCheck {
    pub(crate) fn new(
        verifier: Arc<dyn CatalogVerifier>,
        mode: IndexSignatureMode,
        high_water: Option<PathBuf>,
    ) -> Self {
        Self {
            verifier,
            mode,
            slot: tokio::sync::Mutex::default(),
            warned: AtomicBool::new(false),
            warnings: AtomicUsize::new(0),
            high_water,
            record_problem: std::sync::Mutex::new(None),
            record_warned: AtomicBool::new(false),
        }
    }

    /// Queues the one warning a record that cannot be read or written earns; `report_record_problem` says it.
    fn note_record_problem(&self, what: String) {
        if !self.record_warned.load(Ordering::SeqCst) {
            self.record_problem
                .lock()
                .expect("record problem lock")
                .get_or_insert(what);
        }
    }

    /// Says a queued record warning once per source, unless `quiet`: a probe's line goes to the
    /// debug log and the warning waits for a command that needs the catalog.
    pub(crate) fn report_record_problem(&self, quiet: bool) {
        let mut pending = self.record_problem.lock().expect("record problem lock");
        let Some(what) = pending.as_ref() else { return };
        if quiet {
            log::debug!("{what}");
            return;
        }
        if !self.record_warned.swap(true, Ordering::SeqCst) {
            self.warnings.fetch_add(1, Ordering::SeqCst);
            log::warn!("{what}");
        }
        *pending = None;
    }

    /// How many warnings this source has logged; at most one per process.
    #[cfg(test)]
    pub(crate) fn warnings(&self) -> usize {
        self.warnings.load(Ordering::SeqCst)
    }

    /// Judges `catalog_bytes` with the verifier, then applies the rollback bound to a verified catalog.
    pub(crate) async fn evaluate(&self, catalog_bytes: Vec<u8>, signature: Option<&[u8]>) -> CatalogState {
        let (document, failure) = match self.verifier.check(&catalog_bytes, signature).await {
            CatalogVerdict::Verified { document, .. } => match self.rollback_or_record(&document).await {
                Some(failure) => (None, Some(failure)),
                None => (Some(document), None),
            },
            CatalogVerdict::Unsigned => (None, Some(TrustFailure::Unsigned)),
            CatalogVerdict::Invalid(reason) => (None, Some(TrustFailure::Invalid(reason))),
            CatalogVerdict::PublisherMismatch => (None, Some(TrustFailure::PublisherMismatch)),
            CatalogVerdict::Expired { expires } => (None, Some(TrustFailure::Expired(expires))),
            CatalogVerdict::Unrecognized => (None, Some(TrustFailure::Unrecognized)),
        };
        CatalogState {
            bytes: Some(catalog_bytes),
            document,
            failure,
        }
    }

    /// Refuses a verified catalog whose `expires` is below the highest this machine verified, and
    /// raises the recorded mark otherwise. The file is ephemeral: a failed read or write loses
    /// only the bound, never the verdict.
    async fn rollback_or_record(&self, document: &CatalogDocument) -> Option<TrustFailure> {
        let (path, served) = (self.high_water.as_ref()?, document.expires?);
        let unreadable = |error: &dyn std::fmt::Display| {
            self.note_record_problem(format!(
                "the catalog expiry record '{}' cannot be read ({error}); the rollback bound restarts from this catalog",
                path.display()
            ));
            None
        };
        let recorded = match ocx_util::fs::read_bounded_async(path, HIGH_WATER_CAP).await {
            Ok(bytes) => match serde_json::from_slice::<HighWater>(&bytes) {
                Ok(record) => Some(record.expires_high_water),
                Err(error) => unreadable(&error),
            },
            Err(ocx_util::fs::BoundedReadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                None
            }
            Err(error) => unreadable(&error),
        };
        if let Some(recorded) = recorded.filter(|recorded| *recorded > served) {
            return Some(TrustFailure::Rollback { served, recorded });
        }
        if recorded != Some(served) {
            self.record_high_water(path, served).await;
        }
        None
    }

    async fn record_high_water(&self, path: &std::path::Path, expires: DateTime<Utc>) {
        let Some(parent) = path.parent() else { return };
        let record = HighWater {
            expires_high_water: expires,
        };
        let outcome = async {
            tokio::fs::create_dir_all(parent).await?;
            let bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
            let target = path.to_path_buf();
            tokio::task::spawn_blocking(move || ocx_util::fs::write_private_file_atomic(&target, &bytes))
                .await
                .map_err(std::io::Error::other)?
        }
        .await;
        if let Err(error) = outcome {
            self.note_record_problem(format!(
                "the catalog expiry record '{}' cannot be written ({error}); the rollback check cannot record its bound for this run (index_catalog_rollback protection is off)",
                path.display()
            ));
        }
    }

    /// The failure `root_bytes` (or its absence) earns against a verified `state`; `None` when it
    /// holds, or when the catalog was not verified and so offers nothing to compare against.
    pub(crate) fn judge_root(
        &self,
        state: &CatalogState,
        repository: &str,
        root_bytes: Option<&[u8]>,
    ) -> Option<TrustFailure> {
        let listed = state.document.as_ref()?.packages.get(repository);
        let package = repository.to_string();
        match (listed, root_bytes) {
            (None, None) => None,
            (None, Some(_)) => Some(TrustFailure::PackageNotInCatalog { package }),
            (Some(_), None) => Some(TrustFailure::RootNotFound { package }),
            (Some(digest), Some(bytes)) => (*digest != ocx_oci::Algorithm::Sha256.hash(bytes).to_string())
                .then_some(TrustFailure::RootDigestMismatch { package }),
        }
    }

    /// Applies the mode to `failure`: `enforce` refuses, `warn` logs once per source and carries on.
    ///
    /// A `quiet` caller (a background probe) sends the `warn` line to the debug log and leaves the
    /// once-per-source warning for a caller whose result depends on the catalog; `enforce` still refuses.
    pub(crate) fn surface(&self, registry: &str, failure: TrustFailure, quiet: bool) -> Result<(), IndexTrustError> {
        if self.mode == IndexSignatureMode::Enforce {
            return Err(failure.into_error(registry));
        }
        if quiet {
            log::debug!("index for {registry} is not trusted: {}", failure.reason(registry));
            return Ok(());
        }
        if !self.warned.swap(true, Ordering::SeqCst) {
            self.warnings.fetch_add(1, Ordering::SeqCst);
            log::warn!(
                "index for {registry} is not trusted: {}; set index_signature = \"enforce\" to refuse it or \"off\" to stop checking",
                failure.reason(registry)
            );
        }
        Ok(())
    }
}

/// The per-source record `state/index_catalog/<source>.json` holds.
#[derive(Serialize, Deserialize)]
struct HighWater {
    #[serde(with = "rfc3339_seconds")]
    expires_high_water: DateTime<Utc>,
}

mod rfc3339_seconds {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_rfc3339_opts(SecondsFormat::Secs, true))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<DateTime<Utc>, D::Error> {
        let text = String::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(&text)
            .map(|instant| instant.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::Mutex;

    use super::*;
    use crate::error::Error;
    use crate::{IndexFetch, IndexTransport, OcxIndex, OcxIndexConfig};
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

    pub(crate) const BASE: &str = "https://index.test";
    pub(crate) const NAMESPACE: &str = "ocx.sh";
    pub(crate) const HOLD: std::time::Duration = std::time::Duration::from_secs(1);

    pub(crate) fn catalog_url() -> String {
        format!("{BASE}/c/index.json")
    }
    pub(crate) fn signature_url() -> String {
        format!("{BASE}/c/index.json.sigstore.json")
    }
    pub(crate) fn root_url(repository: &str) -> String {
        format!("{BASE}/p/{repository}.json")
    }

    #[derive(Default)]
    pub(crate) struct Wire {
        bodies: Mutex<HashMap<String, Vec<u8>>>,
        /// What a revalidating request sees instead, as a stale CDN cache would hide it.
        fresh: Mutex<HashMap<String, Vec<u8>>>,
        failing: Mutex<HashMap<String, Option<u16>>>,
        held: Mutex<HashSet<String>>,
        /// `(url, asked_uncached)` in arrival order.
        log: Mutex<Vec<(String, bool)>>,
    }

    /// An in-memory index endpoint that counts what is asked of it.
    #[derive(Clone, Default)]
    pub(crate) struct CountingTransport(Arc<Wire>);

    impl CountingTransport {
        pub(crate) fn put(&self, url: &str, bytes: &[u8]) {
            self.0.bodies.lock().unwrap().insert(url.to_string(), bytes.to_vec());
        }
        /// Serves `bytes` to revalidating requests only; plain requests keep the older body.
        pub(crate) fn put_fresh(&self, url: &str, bytes: &[u8]) {
            self.0.fresh.lock().unwrap().insert(url.to_string(), bytes.to_vec());
        }
        pub(crate) fn uncached_count(&self, url: &str) -> usize {
            self.0
                .log
                .lock()
                .unwrap()
                .iter()
                .filter(|(asked, uncached)| asked == url && *uncached)
                .count()
        }
        pub(crate) fn fail(&self, url: &str) {
            self.0.failing.lock().unwrap().insert(url.to_string(), None);
        }
        /// Answers `url` with a failed request that carried HTTP `status`.
        pub(crate) fn fail_with_status(&self, url: &str, status: u16) {
            self.0.failing.lock().unwrap().insert(url.to_string(), Some(status));
        }
        pub(crate) fn hold(&self, url: &str) {
            self.0.held.lock().unwrap().insert(url.to_string());
        }
        pub(crate) fn count(&self, url: &str) -> usize {
            self.0
                .log
                .lock()
                .unwrap()
                .iter()
                .filter(|(asked, _)| asked == url)
                .count()
        }
        pub(crate) async fn serve(&self, url: &str, uncached: bool) -> crate::error::Result<IndexFetch> {
            self.0.log.lock().unwrap().push((url.to_string(), uncached));
            let held = self.0.held.lock().unwrap().contains(url);
            if held {
                tokio::time::sleep(HOLD).await;
            }
            let failing = self.0.failing.lock().unwrap().get(url).copied();
            if let Some(status) = failing {
                return Err(Error::IndexHttpFailed {
                    url: url.to_string(),
                    status,
                    source: "simulated transport failure".into(),
                });
            }
            let fresh = uncached
                .then(|| self.0.fresh.lock().unwrap().get(url).cloned())
                .flatten();
            let body = fresh.or_else(|| self.0.bodies.lock().unwrap().get(url).cloned());
            Ok(match body {
                Some(bytes) => IndexFetch::Found { bytes },
                None => IndexFetch::NotFound,
            })
        }
    }

    #[async_trait]
    impl IndexTransport for CountingTransport {
        async fn get(&self, url: &str) -> crate::error::Result<IndexFetch> {
            self.serve(url, false).await
        }
        async fn get_uncached(&self, url: &str) -> crate::error::Result<IndexFetch> {
            self.serve(url, true).await
        }
        fn box_clone(&self) -> Box<dyn IndexTransport> {
            Box::new(self.clone())
        }
    }

    /// Answers from a script, then falls back to trusting any catalog that came with a signature.
    #[derive(Default)]
    pub(crate) struct FakeVerifier {
        scripted: Mutex<VecDeque<CatalogVerdict>>,
        calls: AtomicUsize,
    }

    impl FakeVerifier {
        pub(crate) fn script(verdicts: impl IntoIterator<Item = CatalogVerdict>) -> Arc<Self> {
            Arc::new(Self {
                scripted: Mutex::new(verdicts.into_iter().collect()),
                calls: AtomicUsize::new(0),
            })
        }
        pub(crate) fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CatalogVerifier for FakeVerifier {
        async fn check(&self, catalog_bytes: &[u8], signature: Option<&[u8]>) -> CatalogVerdict {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(verdict) = self.scripted.lock().unwrap().pop_front() {
                return verdict;
            }
            match (signature, serde_json::from_slice::<CatalogDocument>(catalog_bytes)) {
                (None, _) => CatalogVerdict::Unsigned,
                (Some(_), Ok(document)) => CatalogVerdict::Verified {
                    document,
                    signer: "fake signer".to_string(),
                },
                (Some(_), Err(error)) => CatalogVerdict::Invalid(error.to_string()),
            }
        }
    }

    pub(crate) fn root_bytes(seed: usize) -> Vec<u8> {
        format!(r#"{{"repository":"oci://ghcr.io/ocx-contrib/tool{seed}","tags":{{}}}}"#).into_bytes()
    }

    pub(crate) fn catalog_listing(roots: &[(&str, &[u8])], expires: Option<&str>) -> Vec<u8> {
        let packages: Vec<String> = roots
            .iter()
            .map(|(repository, bytes)| format!(r#""{repository}":"{}""#, ocx_oci::Algorithm::Sha256.hash(bytes)))
            .collect();
        let expires = expires.map(|at| format!(r#","expires":"{at}""#)).unwrap_or_default();
        format!(
            r#"{{"format_version":1,"packages":{{{}}}{expires}}}"#,
            packages.join(",")
        )
        .into_bytes()
    }

    pub(crate) fn source(wire: &CountingTransport, verifier: Arc<FakeVerifier>, mode: IndexSignatureMode) -> OcxIndex {
        unattached_source(wire).with_catalog_verifier(verifier, mode)
    }

    /// A source with no verifier: the shape `--no-verify` builds.
    pub(crate) fn unattached_source(wire: &CountingTransport) -> OcxIndex {
        OcxIndex::new(OcxIndexConfig {
            transport: Box::new(wire.clone()),
            base_url: BASE.to_string(),
            namespace: NAMESPACE.to_string(),
            client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new()))),
            allow_yanked: false,
            trusted_hosts: Vec::new(),
            insecure_hosts: Vec::new(),
            proxy_rules: ocx_oci::ssrf::ProxyRules::direct(),
        })
    }

    pub(crate) fn package(repository: &str) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(repository, NAMESPACE)
    }

    /// Serves `n` roots, their signed catalog and `config.json`; returns the repositories.
    pub(crate) fn serve_roots(wire: &CountingTransport, n: usize) -> Vec<String> {
        wire.put(&format!("{BASE}/config.json"), br#"{"format_version":1}"#);
        let roots: Vec<(String, Vec<u8>)> = (0..n).map(|i| (format!("ns/tool{i}"), root_bytes(i))).collect();
        for (repository, bytes) in &roots {
            wire.put(&root_url(repository), bytes);
        }
        let listing: Vec<(&str, &[u8])> = roots.iter().map(|(r, b)| (r.as_str(), b.as_slice())).collect();
        wire.put(&catalog_url(), &catalog_listing(&listing, None));
        wire.put(&signature_url(), b"signature");
        roots.into_iter().map(|(repository, _)| repository).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::error::Error;
    use crate::index_impl::IndexImpl as _;
    use crate::{IndexFetch, IndexTransport, OcxIndexConfig};

    // ── the catalog slot ─────────────────────────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn concurrent_root_reads_share_one_catalog_and_one_signature_fetch() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 8);
        wire.hold(&catalog_url());
        let verifier = FakeVerifier::script([]);
        let source = source(&wire, verifier.clone(), IndexSignatureMode::Enforce);

        let identifiers: Vec<_> = repositories.iter().map(|repository| package(repository)).collect();
        let reads = identifiers
            .iter()
            .map(|identifier| source.fetch_root_document(identifier));
        for outcome in futures::future::join_all(reads).await {
            assert!(outcome.expect("a listed, untampered root reads").is_some());
        }

        assert_eq!(wire.count(&catalog_url()), 1, "one catalog fetch for eight roots");
        assert_eq!(wire.count(&signature_url()), 1, "one signature fetch for eight roots");
        assert_eq!(verifier.calls(), 1, "one verdict per source");
    }

    #[tokio::test]
    async fn a_signature_outage_never_fails_a_warn_run_and_fails_an_enforce_run() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.fail(&signature_url());

        let warned = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Warn);
        let outcome = warned.fetch_root_document(&package(&repositories[0])).await;
        assert!(outcome.expect("warn carries on").is_some());
        assert_eq!(
            warned.check.as_ref().map_or(0, |check| check.warnings()),
            1,
            "one WARN, not one per read"
        );

        let enforced = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        let refused = enforced.fetch_root_document(&package(&repositories[0])).await;
        assert!(
            matches!(refused, Err(Error::IndexHttpFailed { .. })),
            "the transport error propagates under enforce: {refused:?}"
        );
    }

    #[tokio::test]
    async fn a_signature_over_the_cap_is_refused_before_the_verifier_sees_it() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.put(&signature_url(), &vec![b'x'; 300 * 1024]);
        let verifier = FakeVerifier::script([]);

        let enforced = source(&wire, verifier.clone(), IndexSignatureMode::Enforce);
        let refused = enforced.fetch_root_document(&package(&repositories[0])).await;
        assert_eq!(slug(&trust(refused.expect_err("refused"))), "index_signature_invalid");
        assert_eq!(verifier.calls(), 0, "an oversize signature is never parsed");
    }

    #[tokio::test]
    async fn a_signature_over_the_cap_only_warns_under_warn() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.put(&signature_url(), &vec![b'x'; 300 * 1024]);
        let verifier = FakeVerifier::script([]);

        let warned = source(&wire, verifier.clone(), IndexSignatureMode::Warn);
        let read = warned.fetch_root_document(&package(&repositories[0])).await;
        assert!(read.expect("warn carries on").is_some());
        assert_eq!(warned.check.as_ref().unwrap().warnings(), 1);
        assert_eq!(verifier.calls(), 0, "an oversize signature is never parsed");
    }

    // ── the root digest check ────────────────────────────────────────────────

    const TAMPERED: &[u8] = br#"{"repository":"oci://evil.example/x","tags":{}}"#;

    fn trust(error: Error) -> IndexTrustError {
        match error {
            Error::Trust(trust) => trust,
            other => panic!("expected a trust refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_tampered_root_is_refused_under_enforce_by_every_reader() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 3);
        for repository in &repositories {
            wire.put(&root_url(repository), TAMPERED);
        }
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);

        // The committing readers.
        let read = source.fetch_root_document(&package(&repositories[0])).await;
        assert!(
            matches!(
                trust(read.expect_err("tampered")),
                IndexTrustError::RootDigestMismatch { .. }
            ),
            "the ordinary root read"
        );
        // `ocx update` and `ocx package prune` read roots that are never committed.
        let uncached = source.fetch_root_uncached(&repositories[1]).await;
        assert!(
            matches!(
                trust(uncached.expect_err("tampered")),
                IndexTrustError::RootDigestMismatch { .. }
            ),
            "the uncached root read"
        );
        let revalidated = source.revalidate_root_document(&package(&repositories[2])).await;
        assert!(
            matches!(
                trust(revalidated.expect_err("tampered")),
                IndexTrustError::RootDigestMismatch { .. }
            ),
            "the revalidating root read"
        );
    }

    #[tokio::test]
    async fn a_root_read_unchecked_never_answers_a_checked_read_of_the_same_source() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.put(&root_url(&repositories[0]), TAMPERED);
        let verifier = FakeVerifier::script([]);
        let checked = source(&wire, verifier.clone(), IndexSignatureMode::Enforce);
        let unchecked = checked.clone().without_catalog_check();

        let (bytes, _) = unchecked
            .fetch_root_document(&package(&repositories[0]))
            .await
            .expect("the unchecked read serves what it is given")
            .expect("served");
        assert_eq!(bytes, TAMPERED);
        assert_eq!(verifier.calls(), 0, "an unchecked read consults no verifier");

        let read = checked.fetch_root_document(&package(&repositories[0])).await;
        assert!(
            matches!(
                trust(read.expect_err("the checked read judges the root itself")),
                IndexTrustError::RootDigestMismatch { .. }
            ),
            "the memo the unchecked clone wrote must not answer a checked read"
        );
        assert!(verifier.calls() >= 1, "the checked read ran the catalog check");
    }

    #[tokio::test(start_paused = true)]
    async fn a_checked_read_never_joins_an_in_flight_unchecked_read_of_the_same_root() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.put(&root_url(&repositories[0]), TAMPERED);
        wire.hold(&root_url(&repositories[0]));
        let verifier = FakeVerifier::script([]);
        let checked = source(&wire, verifier.clone(), IndexSignatureMode::Enforce);
        let unchecked = checked.clone().without_catalog_check();
        let identifier = package(&repositories[0]);

        // `list_tags` reads the root through the coalesced path.
        let (unchecked_read, checked_read) =
            tokio::join!(unchecked.list_tags(&identifier), checked.list_tags(&identifier));

        assert!(
            unchecked_read
                .expect("the unchecked read serves what it is given")
                .is_some(),
            "the unchecked leader read the tampered root"
        );
        let refusal = checked_read.expect_err("the checked read judges the root itself");
        assert!(
            matches!(
                crate::error::coalesced_cause(&refusal),
                Error::Trust(IndexTrustError::RootDigestMismatch { .. })
            ),
            "the unchecked leader's result must not answer a checked waiter: {refusal:?}"
        );
        assert!(verifier.calls() >= 1, "the checked read ran the catalog check");
    }

    #[tokio::test]
    async fn a_root_read_checked_answers_a_later_checked_read_from_the_memo() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        let checked = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        let identifier = package(&repositories[0]);

        checked
            .fetch_root_document(&identifier)
            .await
            .expect("a listed root reads")
            .expect("served");
        let before = wire.count(&root_url(&repositories[0]));
        assert!(before >= 1, "the first read reached the wire");
        checked
            .fetch_root_document(&identifier)
            .await
            .expect("memo hit")
            .expect("served");
        assert_eq!(
            wire.count(&root_url(&repositories[0])),
            before,
            "a checked entry answers a later checked read"
        );
    }

    #[tokio::test]
    async fn a_miss_read_unchecked_never_answers_a_checked_read_of_the_same_source() {
        let wire = CountingTransport::default();
        serve_roots(&wire, 1);
        wire.put(
            &catalog_url(),
            &catalog_listing(&[("ns/tool0", &root_bytes(0)), ("ns/gone", b"never served")], None),
        );
        let checked = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        let unchecked = checked.clone().without_catalog_check();

        assert!(
            unchecked
                .fetch_root_document(&package("ns/gone"))
                .await
                .expect("an unchecked miss is a plain miss")
                .is_none()
        );
        let read = checked.fetch_root_document(&package("ns/gone")).await;
        assert!(
            matches!(
                trust(read.expect_err("the checked read must not take the unchecked miss")),
                IndexTrustError::RootNotFound { .. }
            ),
            "a cached 404 from an unchecked read must not answer a checked read"
        );
    }

    #[tokio::test]
    async fn a_root_read_checked_answers_a_later_unchecked_read_from_the_memo() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        let checked = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        let unchecked = checked.clone().without_catalog_check();

        checked
            .fetch_root_document(&package(&repositories[0]))
            .await
            .expect("a listed root reads")
            .expect("served");
        let before = wire.count(&root_url(&repositories[0]));
        unchecked
            .fetch_root_document(&package(&repositories[0]))
            .await
            .expect("memo hit")
            .expect("served");
        assert_eq!(
            wire.count(&root_url(&repositories[0])),
            before,
            "a checked entry is good enough for an unchecked read"
        );
    }

    #[tokio::test]
    async fn a_tampered_root_passes_under_warn_with_one_warning_per_source() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 2);
        for repository in &repositories {
            wire.put(&root_url(repository), TAMPERED);
        }
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Warn);

        for repository in &repositories {
            let (bytes, _) = source
                .fetch_root_document(&package(repository))
                .await
                .expect("warn carries on")
                .expect("the root is served");
            assert_eq!(bytes, TAMPERED, "warn hands back the bytes it fetched");
        }
        assert_eq!(source.check.as_ref().unwrap().warnings(), 1);
    }

    #[tokio::test]
    async fn a_root_the_signed_catalog_does_not_list_is_refused_under_enforce() {
        let wire = CountingTransport::default();
        serve_roots(&wire, 1);
        wire.put(&root_url("ns/unlisted"), &root_bytes(9));
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);

        let read = source.fetch_root_document(&package("ns/unlisted")).await;
        assert!(matches!(
            trust(read.expect_err("unlisted")),
            IndexTrustError::PackageNotInCatalog { .. }
        ));
    }

    #[tokio::test]
    async fn a_listed_root_the_server_does_not_serve_is_refused_under_enforce_and_warned_under_warn() {
        let wire = CountingTransport::default();
        serve_roots(&wire, 1);
        wire.put(
            &catalog_url(),
            &catalog_listing(&[("ns/tool0", &root_bytes(0)), ("ns/gone", b"never served")], None),
        );

        let enforced = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        let refused = enforced.fetch_root_uncached("ns/gone").await;
        assert!(
            matches!(
                trust(refused.expect_err("listed but absent")),
                IndexTrustError::RootNotFound { .. }
            ),
            "no fall-through to the raw registry source"
        );

        let warned = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Warn);
        assert!(warned.fetch_root_uncached("ns/gone").await.expect("warn").is_none());
        assert_eq!(warned.check.as_ref().unwrap().warnings(), 1);
    }

    #[tokio::test]
    async fn an_unverified_catalog_under_warn_checks_no_digests_and_warns_once() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 2);
        wire.put(&root_url(&repositories[0]), TAMPERED);
        let source = source(
            &wire,
            FakeVerifier::script([CatalogVerdict::Unsigned]),
            IndexSignatureMode::Warn,
        );

        for repository in &repositories {
            assert!(source.fetch_root_uncached(repository).await.expect("warn").is_some());
        }
        assert_eq!(source.check.as_ref().unwrap().warnings(), 1);
    }

    async fn held_failure(source: &crate::OcxIndex, repository: &str) -> TrustFailure {
        assert!(source.fetch_root_uncached(repository).await.expect("warn").is_some());
        let slot = source.check.as_ref().unwrap().slot.lock().await;
        slot.snapshot.as_ref().unwrap().failure.clone().expect("a failure")
    }

    #[tokio::test]
    async fn a_failed_signature_read_under_warn_names_the_fetch_not_a_missing_signature() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.fail(&signature_url());
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Warn);

        let failure = held_failure(&source, &repositories[0]).await;

        let reason = failure.reason(NAMESPACE);
        assert!(reason.contains("catalog signature could not be fetched"), "{reason}");
        assert!(!reason.contains("serves no catalog signature"), "{reason}");
        assert_eq!(source.check.as_ref().unwrap().warnings(), 1);
    }

    #[tokio::test]
    async fn a_failed_catalog_read_under_warn_names_the_fetch_not_a_missing_signature() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.fail(&catalog_url());
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Warn);

        let failure = held_failure(&source, &repositories[0]).await;

        let reason = failure.reason(NAMESPACE);
        assert!(reason.contains("catalog could not be fetched"), "{reason}");
        assert!(!reason.contains("serves no catalog signature"), "{reason}");
    }

    #[tokio::test]
    async fn a_genuinely_unsigned_catalog_still_reads_as_serving_no_signature() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        let source = source(
            &wire,
            FakeVerifier::script([CatalogVerdict::Unsigned]),
            IndexSignatureMode::Warn,
        );

        let failure = held_failure(&source, &repositories[0]).await;

        assert!(failure.reason(NAMESPACE).contains("serves no catalog signature"));
    }

    #[tokio::test]
    async fn a_failed_signature_read_under_enforce_still_propagates_the_transport_error() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.fail(&signature_url());
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);

        let refused = source.fetch_root_uncached(&repositories[0]).await;

        assert!(matches!(
            refused.expect_err("enforce"),
            crate::error::Error::IndexHttpFailed { .. }
        ));
    }

    #[test]
    fn the_short_cause_carries_a_status_and_never_a_url() {
        let error = crate::error::Error::IndexHttpFailed {
            url: "https://user:secret@idx.test/c/index.json".to_string(),
            status: Some(500),
            source: "https://user:secret@idx.test".into(),
        };
        let cause = error.short_transport_cause();
        assert_eq!(cause, "HTTP status 500");
    }

    #[test]
    fn a_body_over_the_cap_names_the_cap_not_the_status() {
        let error = crate::error::Error::IndexHttpFailed {
            url: "https://idx.test/c/index.json".to_string(),
            status: Some(200),
            source: "response body exceeds the 262144-byte document cap".into(),
        };
        let reason = TrustFailure::Unfetched {
            what: "catalog signature",
            error: ArcError::from(error),
        }
        .reason(NAMESPACE);
        assert!(reason.contains("262144-byte document cap"), "{reason}");
        assert!(!reason.contains("HTTP status"), "{reason}");
    }

    #[tokio::test]
    async fn a_quiet_view_spends_no_warning_and_leaves_it_for_the_caller_that_needs_the_catalog() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        let source = source(
            &wire,
            FakeVerifier::script([CatalogVerdict::Unsigned]),
            IndexSignatureMode::Warn,
        );

        let quiet = source.clone().quiet();
        assert!(
            quiet
                .fetch_root_uncached(&repositories[0])
                .await
                .expect("quiet")
                .is_some()
        );
        assert_eq!(source.check.as_ref().unwrap().warnings(), 0, "the probe warned");

        assert!(
            source
                .fetch_root_uncached(&repositories[0])
                .await
                .expect("warn")
                .is_some()
        );
        assert_eq!(source.check.as_ref().unwrap().warnings(), 1, "the command still warns");
    }

    #[tokio::test]
    async fn a_quiet_view_still_refuses_under_enforce() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        let source = source(
            &wire,
            FakeVerifier::script([CatalogVerdict::Unsigned]),
            IndexSignatureMode::Enforce,
        )
        .quiet();

        let refused = source.fetch_root_uncached(&repositories[0]).await;
        assert!(matches!(
            trust(refused.expect_err("enforce")),
            IndexTrustError::SignatureNotFound { .. }
        ));
    }

    // ── the one uncached refetch ─────────────────────────────────────────────

    #[tokio::test]
    async fn a_stale_cached_catalog_is_replaced_once_by_an_uncached_read() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        // The cache still holds the previous generation, which lists a different root.
        wire.put(
            &catalog_url(),
            &catalog_listing(&[(repositories[0].as_str(), b"old root")], None),
        );
        wire.put_fresh(
            &catalog_url(),
            &catalog_listing(&[(repositories[0].as_str(), &root_bytes(0))], None),
        );
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);

        let (bytes, _) = source
            .fetch_root_document(&package(&repositories[0]))
            .await
            .expect("the fresh catalog vouches for the served root")
            .expect("served");
        assert_eq!(bytes, root_bytes(0));
        assert_eq!(wire.uncached_count(&catalog_url()), 1);
        assert_eq!(wire.uncached_count(&signature_url()), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_mismatches_spend_one_refetch_per_source() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 5);
        for repository in &repositories {
            wire.put(&root_url(repository), TAMPERED);
        }
        wire.hold(&catalog_url());
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);

        let identifiers: Vec<_> = repositories.iter().map(|repository| package(repository)).collect();
        let reads = identifiers
            .iter()
            .map(|identifier| source.fetch_root_document(identifier));
        for outcome in futures::future::join_all(reads).await {
            assert!(matches!(
                trust(outcome.expect_err("tampered")),
                IndexTrustError::RootDigestMismatch { .. }
            ));
        }
        assert_eq!(
            wire.uncached_count(&catalog_url()),
            1,
            "one refetch, however many roots mismatch"
        );
        assert_eq!(wire.uncached_count(&signature_url()), 1);
    }

    /// The server switches catalog generations while two roots are read: each root is judged
    /// against one snapshot, never against the first verdict paired with the second bytes.
    #[tokio::test(start_paused = true)]
    async fn a_generation_switch_mid_run_never_mixes_a_root_with_the_wrong_snapshot() {
        let wire = CountingTransport::default();
        wire.put(&format!("{BASE}/config.json"), br#"{"format_version":1}"#);
        let (stable, moved) = ("ns/stable", "ns/moved");
        let (old_moved, new_moved) = (root_bytes(1), root_bytes(2));
        wire.put(&root_url(stable), &root_bytes(0));
        wire.put(&root_url(moved), &new_moved);
        wire.put(&signature_url(), b"signature");
        wire.put(
            &catalog_url(),
            &catalog_listing(&[(stable, &root_bytes(0)), (moved, &old_moved)], None),
        );
        wire.put_fresh(
            &catalog_url(),
            &catalog_listing(&[(stable, &root_bytes(0)), (moved, &new_moved)], None),
        );
        wire.hold(&catalog_url());
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);

        let (stable_id, moved_id) = (package(stable), package(moved));
        let (first, second) = futures::future::join(
            source.fetch_root_document(&stable_id),
            source.fetch_root_document(&moved_id),
        )
        .await;
        assert_eq!(first.expect("stable").expect("served").0, root_bytes(0));
        assert_eq!(second.expect("moved").expect("served").0, new_moved);
        assert_eq!(wire.uncached_count(&catalog_url()), 1);
    }

    #[tokio::test]
    async fn the_second_answer_is_final_for_the_process() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 2);
        wire.put(&root_url(&repositories[0]), TAMPERED);
        wire.put(&root_url(&repositories[1]), TAMPERED);
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);

        for repository in &repositories {
            let outcome = source.fetch_root_document(&package(repository)).await;
            assert!(outcome.is_err());
        }
        assert_eq!(
            wire.uncached_count(&catalog_url()),
            1,
            "a later mismatch buys no second refetch"
        );
    }

    // ── the mode table ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn off_asks_for_no_catalog_and_no_signature_and_checks_nothing() {
        let wire = CountingTransport::default();
        let repositories = serve_roots(&wire, 1);
        wire.put(&root_url(&repositories[0]), TAMPERED);
        let verifier = FakeVerifier::script([]);
        let source = source(&wire, verifier.clone(), IndexSignatureMode::Off);

        let (bytes, _) = source
            .fetch_root_document(&package(&repositories[0]))
            .await
            .expect("off behaves as before")
            .expect("served");
        assert_eq!(bytes, TAMPERED);
        assert_eq!(wire.count(&catalog_url()), 0);
        assert_eq!(wire.count(&signature_url()), 0);
        assert_eq!(verifier.calls(), 0);
    }

    fn failing_verdicts() -> Vec<(&'static str, CatalogVerdict)> {
        vec![
            ("index_signature_not_found", CatalogVerdict::Unsigned),
            ("index_signature_invalid", CatalogVerdict::Invalid("bad".to_string())),
            ("index_publisher_mismatch", CatalogVerdict::PublisherMismatch),
            ("index_publishers_unrecognized", CatalogVerdict::Unrecognized),
            ("index_catalog_expired", CatalogVerdict::Expired { expires: None }),
        ]
    }

    fn slug(error: &IndexTrustError) -> &'static str {
        match error {
            IndexTrustError::SignatureNotFound { .. } => "index_signature_not_found",
            IndexTrustError::SignatureInvalid { .. } => "index_signature_invalid",
            IndexTrustError::PublisherMismatch { .. } => "index_publisher_mismatch",
            IndexTrustError::PublishersUnrecognized { .. } => "index_publishers_unrecognized",
            IndexTrustError::CatalogExpired { .. } => "index_catalog_expired",
            IndexTrustError::CatalogRollback { .. } => "index_catalog_rollback",
            IndexTrustError::RootDigestMismatch { .. } => "index_root_digest_mismatch",
            IndexTrustError::PackageNotInCatalog { .. } => "index_package_not_in_catalog",
            IndexTrustError::RootNotFound { .. } => "index_root_not_found",
        }
    }

    #[tokio::test]
    async fn enforce_refuses_every_failing_verdict_with_its_own_slug() {
        for (expected, verdict) in failing_verdicts() {
            let wire = CountingTransport::default();
            let repositories = serve_roots(&wire, 1);
            let source = source(
                &wire,
                FakeVerifier::script([verdict.clone(), verdict]),
                IndexSignatureMode::Enforce,
            );

            let refused = source.fetch_root_document(&package(&repositories[0])).await;
            assert_eq!(slug(&trust(refused.expect_err("refused"))), expected);
        }
    }

    #[tokio::test]
    async fn warn_continues_on_every_failing_verdict_with_exactly_one_warning() {
        for (name, verdict) in failing_verdicts() {
            let wire = CountingTransport::default();
            let repositories = serve_roots(&wire, 2);
            let source = source(
                &wire,
                FakeVerifier::script([verdict.clone(), verdict]),
                IndexSignatureMode::Warn,
            );

            for repository in &repositories {
                let read = source.fetch_root_document(&package(repository)).await;
                assert!(
                    read.expect(name).is_some(),
                    "{name}: warn carries on with the bytes it fetched"
                );
            }
            assert_eq!(source.check.as_ref().unwrap().warnings(), 1, "{name}");
        }
    }

    #[tokio::test]
    async fn the_strict_catalog_read_honours_the_verdict_and_keeps_its_absent_catalog_error_under_warn() {
        let wire = CountingTransport::default();
        serve_roots(&wire, 2);
        let verified = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        assert_eq!(verified.fetch_catalog_strict().await.expect("verified").len(), 2);

        let unsigned = source(
            &wire,
            FakeVerifier::script([CatalogVerdict::Unsigned]),
            IndexSignatureMode::Enforce,
        );
        let refused = unsigned.fetch_catalog_strict().await;
        assert_eq!(slug(&trust(refused.expect_err("refused"))), "index_signature_not_found");

        let absent = CountingTransport::default();
        absent.put(&format!("{BASE}/config.json"), br#"{"format_version":1}"#);
        let missing = source(&absent, FakeVerifier::script([]), IndexSignatureMode::Warn);
        assert!(matches!(
            missing.fetch_catalog_strict().await,
            Err(Error::CatalogDocumentAbsent { .. })
        ));

        // Under enforce the absent catalog is the same refusal the root read raises.
        let enforced = source(&absent, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        let refused = enforced.fetch_catalog_strict().await;
        assert_eq!(slug(&trust(refused.expect_err("refused"))), "index_signature_not_found");
    }

    /// Serves the catalog as absent for its first read and as a real document afterwards,
    /// so a second, unchecked read would take unsigned data.
    struct LateCatalog {
        inner: CountingTransport,
        catalog_reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl LateCatalog {
        async fn serve(&self, url: &str, uncached: bool) -> crate::error::Result<IndexFetch> {
            if url == catalog_url() && self.catalog_reads.fetch_add(1, Ordering::SeqCst) < 1 {
                return Ok(IndexFetch::NotFound);
            }
            self.inner.serve(url, uncached).await
        }
    }

    #[async_trait]
    impl IndexTransport for LateCatalog {
        async fn get(&self, url: &str) -> crate::error::Result<IndexFetch> {
            self.serve(url, false).await
        }
        async fn get_uncached(&self, url: &str) -> crate::error::Result<IndexFetch> {
            self.serve(url, true).await
        }
        fn box_clone(&self) -> Box<dyn IndexTransport> {
            Box::new(LateCatalog {
                inner: self.inner.clone(),
                catalog_reads: self.catalog_reads.clone(),
            })
        }
    }

    fn late_catalog_source(
        mode: IndexSignatureMode,
    ) -> (crate::ocx_index::OcxIndex, Arc<std::sync::atomic::AtomicUsize>) {
        let wire = CountingTransport::default();
        serve_roots(&wire, 2);
        let catalog_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let transport = LateCatalog {
            inner: wire,
            catalog_reads: catalog_reads.clone(),
        };
        let source = crate::ocx_index::OcxIndex::new(OcxIndexConfig {
            transport: Box::new(transport),
            base_url: BASE.to_string(),
            namespace: NAMESPACE.to_string(),
            client: ocx_oci::Client::with_transport(Box::new(ocx_oci::client::test_transport::StubTransport::new(
                ocx_oci::client::test_transport::StubTransportData::new(),
            ))),
            allow_yanked: false,
            trusted_hosts: Vec::new(),
            insecure_hosts: Vec::new(),
            proxy_rules: ocx_oci::ssrf::ProxyRules::direct(),
        })
        .with_catalog_verifier(FakeVerifier::script([]), mode);
        (source, catalog_reads)
    }

    #[tokio::test]
    async fn the_strict_catalog_read_never_takes_an_unchecked_second_read_under_enforce() {
        let (source, catalog_reads) = late_catalog_source(IndexSignatureMode::Enforce);

        let refused = source.fetch_catalog_strict().await;
        assert_eq!(slug(&trust(refused.expect_err("refused"))), "index_signature_not_found");
        assert_eq!(
            catalog_reads.load(Ordering::SeqCst),
            1,
            "no read beyond the checked one"
        );
    }

    #[tokio::test]
    async fn listing_the_repositories_never_takes_unsigned_data_under_enforce() {
        let (source, _) = late_catalog_source(IndexSignatureMode::Enforce);

        let refused = source.list_repositories(NAMESPACE).await;
        assert_eq!(slug(&trust(refused.expect_err("refused"))), "index_signature_not_found");
    }

    #[tokio::test]
    async fn a_remote_listing_through_the_chain_never_takes_unsigned_data_under_enforce() {
        let (source, _) = late_catalog_source(IndexSignatureMode::Enforce);
        let directory = tempfile::tempdir().unwrap();
        let local = crate::LocalIndex::new(crate::LocalConfig {
            index_store: crate::IndexStore::new(directory.path().join("index")),
        });
        let chain =
            crate::Index::from_chained(local, vec![crate::Index::from_source(source)], crate::ChainMode::Remote);

        let refused = chain.list_repositories(NAMESPACE).await;
        assert_eq!(slug(&trust(refused.expect_err("refused"))), "index_signature_not_found");
    }

    #[tokio::test]
    async fn the_strict_catalog_read_under_warn_warns_and_takes_no_unchecked_read() {
        let (source, catalog_reads) = late_catalog_source(IndexSignatureMode::Warn);

        let absent = source.fetch_catalog_strict().await;
        assert!(matches!(absent, Err(Error::CatalogDocumentAbsent { .. })), "{absent:?}");
        assert_eq!(source.check.as_ref().unwrap().warnings(), 1);
        assert_eq!(catalog_reads.load(Ordering::SeqCst), 1);
    }

    /// The exit code and `error.detail` slug the error exits with, read through the coalescing wrapper.
    fn exit_of(error: &Error) -> (ocx_exit::ExitCode, &'static str) {
        use ocx_exit::{ClassifyErrorKind as _, ClassifyExitCode as _, Detail};
        let cause = crate::error::coalesced_cause(error);
        let slug = match cause.kind_detail() {
            Detail::Fixed(entry) | Detail::Chain { fallback: entry, .. } => entry.slug,
        };
        (cause.classify().expect("the error declares a code"), slug)
    }

    #[tokio::test]
    async fn the_strict_catalog_read_keeps_the_transport_exit_when_the_fetch_fails() {
        for mode in [IndexSignatureMode::Warn, IndexSignatureMode::Enforce] {
            let wire = CountingTransport::default();
            serve_roots(&wire, 1);
            wire.fail_with_status(&catalog_url(), 503);
            let source = source(&wire, FakeVerifier::script([]), mode);

            let refused = source
                .fetch_catalog_strict()
                .await
                .expect_err("the catalog is unreadable");
            assert_eq!(
                exit_of(&refused),
                (ocx_exit::ExitCode::TempFail, "index_http_transient"),
                "{mode:?}: a transient failure is retryable, never `absent` or a missing signature: {refused:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_served_404_catalog_still_exits_catalog_document_absent_under_warn() {
        let wire = CountingTransport::default();
        let source = source(&wire, FakeVerifier::script([]), IndexSignatureMode::Warn);

        let refused = source.fetch_catalog_strict().await.expect_err("no catalog is served");
        assert_eq!(
            exit_of(&refused),
            (ocx_exit::ExitCode::Unavailable, "catalog_document_absent")
        );
    }

    #[tokio::test]
    async fn an_oversized_catalog_on_the_strict_path_exits_unavailable_under_both_modes() {
        for mode in [IndexSignatureMode::Warn, IndexSignatureMode::Enforce] {
            let wire = CountingTransport::default();
            serve_roots(&wire, 1);
            wire.put(
                &catalog_url(),
                &vec![b' '; ocx_oci::client::MAX_INDEX_DOCUMENT_BYTES + 1],
            );
            let source = source(&wire, FakeVerifier::script([]), mode);

            let refused = source.fetch_catalog_strict().await.expect_err("over the cap");
            assert_eq!(
                exit_of(&refused).0,
                ocx_exit::ExitCode::Unavailable,
                "{mode:?}: {refused:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_catalog_with_its_signature_absent_is_refused_by_every_listing_entry_point() {
        let wire = CountingTransport::default();
        serve_roots(&wire, 2);
        let unsigned = CountingTransport::default();
        unsigned.put(&format!("{BASE}/config.json"), br#"{"format_version":1}"#);
        unsigned.put(&catalog_url(), &catalog_listing(&[("ns/tool0", &root_bytes(0))], None));

        let strict = source(&unsigned, FakeVerifier::script([]), IndexSignatureMode::Enforce);
        let refused = strict.fetch_catalog_strict().await;
        assert_eq!(slug(&trust(refused.expect_err("refused"))), "index_signature_not_found");
        let listed = source(&unsigned, FakeVerifier::script([]), IndexSignatureMode::Enforce)
            .list_repositories(NAMESPACE)
            .await;
        assert_eq!(slug(&trust(listed.expect_err("refused"))), "index_signature_not_found");
    }

    // ── the expiry high-water mark ───────────────────────────────────────────

    const LATER: &str = "2031-01-01T00:00:00Z";
    const EARLIER: &str = "2030-01-01T00:00:00Z";

    /// One root and a catalog that expires at `expires`; returns the repository.
    fn serve_expiring(wire: &CountingTransport, expires: &str) -> String {
        let repository = serve_roots(wire, 1).remove(0);
        let root = root_bytes(0);
        wire.put(&catalog_url(), &catalog_listing(&[(&repository, &root)], Some(expires)));
        repository
    }

    fn recorded(path: &std::path::Path) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("record is json");
        value["expires_high_water"].as_str().map(str::to_string)
    }

    fn record(path: &std::path::Path, expires: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!(r#"{{"expires_high_water":"{expires}"}}"#)).unwrap();
    }

    fn with_mark(
        wire: &CountingTransport,
        verifier: Arc<FakeVerifier>,
        mode: IndexSignatureMode,
        path: &std::path::Path,
    ) -> crate::ocx_index::OcxIndex {
        unattached_source(wire)
            .with_expiry_high_water_file(path.to_path_buf())
            .with_catalog_verifier(verifier, mode)
    }

    #[tokio::test]
    async fn a_verified_catalog_records_its_expiry_and_a_later_one_raises_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/index_catalog/source.json");
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, EARLIER);

        let first = with_mark(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce, &path);
        first
            .fetch_root_document(&package(&repository))
            .await
            .expect("verified")
            .expect("served");
        assert_eq!(recorded(&path).as_deref(), Some(EARLIER));

        let root = root_bytes(0);
        wire.put(&catalog_url(), &catalog_listing(&[(&repository, &root)], Some(LATER)));
        let second = with_mark(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce, &path);
        second
            .fetch_root_document(&package(&repository))
            .await
            .expect("verified")
            .expect("served");
        assert_eq!(recorded(&path).as_deref(), Some(LATER));
    }

    #[tokio::test]
    async fn a_catalog_expiring_exactly_at_the_recorded_mark_is_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index_catalog/source.json");
        record(&path, EARLIER);
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, EARLIER);
        let source = with_mark(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce, &path);

        let read = source.fetch_root_document(&package(&repository)).await;
        assert!(read.expect("equal is not a rollback").is_some());
        assert_eq!(recorded(&path).as_deref(), Some(EARLIER));
    }

    #[tokio::test]
    async fn a_rollback_is_refused_under_enforce_and_leaves_the_mark_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index_catalog/source.json");
        record(&path, LATER);
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, EARLIER);
        let source = with_mark(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce, &path);

        let refused = source
            .fetch_root_document(&package(&repository))
            .await
            .expect_err("refused");
        assert_eq!(slug(&trust(refused)), "index_catalog_rollback");
        assert_eq!(recorded(&path).as_deref(), Some(LATER));
    }

    #[tokio::test]
    async fn a_rollback_only_warns_under_warn_and_leaves_the_mark_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index_catalog/source.json");
        record(&path, LATER);
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, EARLIER);
        let source = with_mark(&wire, FakeVerifier::script([]), IndexSignatureMode::Warn, &path);

        let read = source.fetch_root_document(&package(&repository)).await;
        assert!(read.expect("warn carries on").is_some());
        assert_eq!(source.check.as_ref().unwrap().warnings(), 1);
        assert_eq!(recorded(&path).as_deref(), Some(LATER));
    }

    #[tokio::test]
    async fn only_a_verified_verdict_writes_the_mark() {
        for verdict in [
            CatalogVerdict::Unsigned,
            CatalogVerdict::Invalid("bad".to_string()),
            CatalogVerdict::PublisherMismatch,
            CatalogVerdict::Unrecognized,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("index_catalog/source.json");
            let wire = CountingTransport::default();
            let repository = serve_expiring(&wire, LATER);
            let source = with_mark(
                &wire,
                FakeVerifier::script([verdict.clone(), verdict]),
                IndexSignatureMode::Warn,
                &path,
            );

            source
                .fetch_root_document(&package(&repository))
                .await
                .expect("warn carries on");
            assert!(!path.exists(), "an unverified catalog must not move the mark");
        }
    }

    #[tokio::test]
    async fn off_and_a_source_with_no_verifier_never_touch_the_mark() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index_catalog/source.json");
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, LATER);

        let off = with_mark(&wire, FakeVerifier::script([]), IndexSignatureMode::Off, &path);
        off.fetch_root_document(&package(&repository))
            .await
            .expect("off")
            .expect("served");
        let unverified = unattached_source(&wire).with_expiry_high_water_file(path.clone());
        unverified
            .fetch_root_document(&package(&repository))
            .await
            .expect("no verify")
            .expect("served");
        assert!(!path.exists());

        record(&path, EARLIER);
        off.fetch_root_document(&package(&repository)).await.expect("off");
        assert_eq!(recorded(&path).as_deref(), Some(EARLIER));
    }

    #[tokio::test]
    async fn a_record_that_cannot_be_written_warns_once_and_the_command_carries_on() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("index_catalog");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("source.json");
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, LATER);

        for mode in [IndexSignatureMode::Warn, IndexSignatureMode::Enforce] {
            let source = with_mark(&wire, FakeVerifier::script([]), mode, &path);
            for _ in 0..2 {
                source
                    .fetch_root_document(&package(&repository))
                    .await
                    .expect("a lost bound never refuses")
                    .expect("served");
            }
            assert_eq!(source.check.as_ref().unwrap().warnings(), 1, "{mode:?}");
        }
    }

    #[tokio::test]
    async fn an_unreadable_record_warns_once_is_reset_and_rewritten() {
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, EARLIER);
        let oversized = vec![b' '; 70 * 1024];
        let garbage: [&[u8]; 4] = [b"garbage", b"", br#"{"expires_high_water":7}"#, &oversized];
        for bytes in garbage {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("index_catalog/source.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            let source = with_mark(&wire, FakeVerifier::script([]), IndexSignatureMode::Enforce, &path);
            source
                .fetch_root_document(&package(&repository))
                .await
                .expect("reset, not refused")
                .expect("served");
            assert_eq!(source.check.as_ref().unwrap().warnings(), 1, "{} bytes", bytes.len());
            assert_eq!(recorded(&path).as_deref(), Some(EARLIER));
        }
    }

    #[tokio::test]
    async fn a_missing_record_is_silent_and_a_quiet_view_leaves_the_record_warning_for_the_command() {
        let directory = tempfile::tempdir().unwrap();
        let wire = CountingTransport::default();
        let repository = serve_expiring(&wire, LATER);

        let fresh = with_mark(
            &wire,
            FakeVerifier::script([]),
            IndexSignatureMode::Warn,
            &directory.path().join("a/x.json"),
        );
        fresh.fetch_root_uncached(&repository).await.unwrap().unwrap();
        assert_eq!(fresh.check.as_ref().unwrap().warnings(), 0);

        let blocker = directory.path().join("b");
        std::fs::write(&blocker, b"file").unwrap();
        let source = with_mark(
            &wire,
            FakeVerifier::script([]),
            IndexSignatureMode::Warn,
            &blocker.join("x.json"),
        );
        source
            .clone()
            .quiet()
            .fetch_root_uncached(&repository)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(source.check.as_ref().unwrap().warnings(), 0, "the probe warned");
        source.fetch_root_uncached(&repository).await.unwrap().unwrap();
        assert_eq!(source.check.as_ref().unwrap().warnings(), 1, "the command still warns");
    }
}
