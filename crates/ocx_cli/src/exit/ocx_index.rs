// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the `ocx_index` error family.

use ocx_exit::ExitCode;

use ocx_index::error::Error as OciIndexError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for OciIndexError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::RemoteManifestNotFound(_) | Self::NoIndexableTag(_) | Self::NotInIndex { .. } => ExitCode::NotFound,
            Self::NestedImageIndex { .. } => ExitCode::DataError,
            Self::Store(error) => return error.classify(),
            Self::OciClient(error) => return error.classify(),
            Self::Digest(error) => return error.classify(),
            Self::PinnedIdentifier(error) => return error.classify(),
            Self::File(error) => return error.classify(),
            Self::PathInvalid(_) => ExitCode::Failure,
            Self::SerializationFailure(_) => ExitCode::DataError,
            // The full chain walker, not a single-hop `classify()`, or nested causes go unclassified.
            Self::SourceWalkFailed(arc) | Self::SourceFetchFailed(arc) => {
                return Some(super::classify_library_error(arc.as_error()));
            }
            // `None`, not `Some(Failure)`: a `Some` ends the walk before the leader's typed source, so waiters would exit 1.
            Self::SingleflightFailed(_) => return None,
            Self::PolicyResolutionBlocked { .. } => ExitCode::PolicyBlocked,
            Self::UnsupportedIndexFormat { .. }
            | Self::DispatchObjectDigestMismatch { .. }
            | Self::WalkedDigestMismatch { .. }
            | Self::YankedRefused { .. }
            | Self::MalformedPhysicalRef { .. }
            | Self::RootRepositoryMismatch { .. }
            | Self::MalformedCatalogKey { .. }
            | Self::InvalidImageIndex(_)
            | Self::MalformedIndexDocument { .. } => ExitCode::DataError,
            Self::IndexHttpFailed { .. } if self.is_transient_transport() => ExitCode::TempFail,
            Self::IndexHttpFailed { .. } | Self::CatalogDocumentAbsent { .. } => ExitCode::Unavailable,
            Self::PlainHttpIndexNotAllowed { .. } | Self::InvalidIndexUrl { .. } => ExitCode::ConfigError,
            Self::Ssrf { source, .. } => return source.classify(),
        })
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, OciIndexError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_exit::ExitCode;

    // ── moved from ocx_lib::file_structure::index_store with the impl ──

    // ── moved from ocx_lib::oci::index::chained_index with the impl ──

    // ── moved from ocx_lib::oci::index::error with the impl ──

    // ── moved from ocx_lib::oci::index::local_index with the impl ──

    // ── moved from ocx_lib::oci::index::ocx_index with the impl ──

    // ── moved from ocx_lib::oci::index::regenerate with the impl ──

    // ── moved from ocx_lib::oci::index::file_transport with the impl ──

    // ── moved from ocx_lib::oci::index::ocx_index with the impl ──

    // ── moved from ocx_lib::oci::index::file_transport with the impl ──

    // ── moved from ocx_lib::oci::index::error with the impl ──

    /// A coordination failure with no leader error to defer to still carries its
    /// own meaning: a singleflight timeout is transient, so it must reach
    /// `TempFail` (75) — the retryable class — rather than collapsing to a
    /// generic failure the way the terminal arm did.
    #[test]
    fn singleflight_timeout_classifies_as_temp_fail() {
        let error = OciIndexError::SingleflightFailed(ocx_util::singleflight::Error::Timeout);
        assert_eq!(crate::exit::classify_library_error(&error), ExitCode::TempFail);
    }

    /// An SSRF *resolution* failure classifies to `TempFail` (75), not
    /// `ConfigError` (78): no `trusted_hosts` entry fixes a host that does not
    /// resolve, and a DNS failure at connect time is already 75, so the same
    /// host must not exit 69 or 75 depending on which check hit first.
    #[test]
    fn ssrf_resolution_classifies_as_temp_fail() {
        let error = OciIndexError::Ssrf {
            source: ocx_oci::ssrf::PhysicalDialRefused {
                namespace: "ocx.sh".to_string(),
                source: ocx_oci::ssrf::SsrfError::Resolution {
                    host: "no-such-registry.invalid".to_string(),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "name or service not known"),
                },
            },
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }

    fn index_http_failed(
        status: Option<u16>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> OciIndexError {
        OciIndexError::IndexHttpFailed {
            url: "https://index.ocx.sh/p/acme/tool.json".to_string(),
            status,
            source: source.into(),
        }
    }

    /// A connect to a port the OS just handed back: refused, with no network.
    async fn refused_connect() -> reqwest::Error {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("bind a free loopback port")
            .port();
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a stock client builds")
            .get(format!("http://127.0.0.1:{port}/p/acme/tool.json"))
            .send()
            .await
            .expect_err("a connect to a closed port fails");
        assert!(error.is_connect(), "precondition: a connect failure, got {error:?}");
        error
    }

    /// A peer that accepts and drops: the retry ladder retries it, the exit code calls it terminal.
    async fn hangup_after_connect() -> reqwest::Error {
        let addr = ocx_test_support::net::serve_hangup().await;
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a stock client builds")
            .get(format!("http://{addr}/p/acme/tool.json"))
            .send()
            .await
            .expect_err("a dropped connection fails");
        assert!(
            ocx_oci::transport_policy::is_retryable_transport_error(&error),
            "precondition: the ladder retries a hang-up, got {error:?}"
        );
        error
    }

    /// A rerun may succeed: a refused connect, or a 408/429/502/503/504 answer.
    #[tokio::test]
    async fn transient_index_failures_classify_as_temp_fail() {
        for status in [408, 429, 502, 503, 504] {
            let error = index_http_failed(Some(status), format!("unexpected status {status}"));
            assert_eq!(error.classify(), Some(ExitCode::TempFail), "HTTP {status}");
        }
        assert_eq!(
            index_http_failed(None, refused_connect().await).classify(),
            Some(ExitCode::TempFail),
            "a refused connect"
        );
    }

    /// A rerun answers the same: a terminal status, a hang-up after connect, a
    /// refused certificate, a `file://` refusal or the cap — each stays `Unavailable` (69).
    #[tokio::test]
    async fn terminal_index_failures_classify_as_unavailable() {
        for status in [403, 500, 501] {
            let error = index_http_failed(Some(status), format!("unexpected status {status}"));
            assert_eq!(error.classify(), Some(ExitCode::Unavailable), "HTTP {status}");
        }
        // The hint wraps only a refused certificate; wrapping a connect error
        // proves the wrapper alone decides, whatever it carries.
        let hinted = ocx_oci::transport_policy::UntrustedCertificateHint::for_url(None, refused_connect().await);
        let hangup = hangup_after_connect().await;
        let cases: Vec<(&str, OciIndexError)> = vec![
            ("hang-up after connect", index_http_failed(None, hangup)),
            ("refused certificate", index_http_failed(None, hinted)),
            (
                "file-transport refusal",
                index_http_failed(
                    None,
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
                ),
            ),
            ("network refused", index_http_failed(None, "network access refused")),
            (
                "oversize body",
                index_http_failed(Some(200), "response body exceeds the cap"),
            ),
        ];
        for (what, error) in cases {
            assert_eq!(error.classify(), Some(ExitCode::Unavailable), "{what}");
        }
    }

    // recovered from ocx_index::error
    /// The exit code of a concurrent resolve must not depend on who reached the
    /// key first.
    ///
    /// Both errors are built in the exact shape `walk_chain` produces: the
    /// leader propagates `SourceWalkFailed(ArcError)`, and the waiter receives
    /// that same error broadcast inside
    /// `SingleflightFailed(Failed(SharedError(..)))`. Testing only the leader
    /// cannot see the bug — the leader arm was always correct; it was the
    /// waiter's terminal `Some(Failure)` that ended the walk before the typed
    /// error underneath was ever reached.
    ///
    /// Chain-walked via [`crate::exit::classify_library_error`] rather than a single-hop
    /// `classify()`, because the whole mechanism is the walk continuing through
    /// `#[source]`.
    #[test]
    fn leader_and_waiter_classify_a_source_walk_failure_identically() {
        use ocx_util::singleflight;

        // One case per exit-code class a source walk realistically produces:
        // a rejected credential, malformed publisher data, an index that refuses
        // and one that is briefly down.
        // E1: a source walk now carries the index tier's own error, so the
        // cases are built at that type. The client case goes through the tier's
        // `OciClient` variant, which reconstructs as `ocx_lib::Error::OciClient`
        // at the CLI boundary — same error, same code, one wrapper fewer.
        let cases: Vec<(OciIndexError, ExitCode)> = vec![
            (
                OciIndexError::OciClient(ocx_oci::client::error::ClientError::Authentication(Box::new(
                    std::io::Error::other("token refused"),
                ))),
                ExitCode::AuthError,
            ),
            (
                OciIndexError::YankedRefused {
                    identifier: "ocx.sh/kitware/cmake:3.28".to_string(),
                },
                ExitCode::DataError,
            ),
            (
                OciIndexError::IndexHttpFailed {
                    url: "https://index.ocx.sh/c/index.json".to_string(),
                    status: Some(403),
                    source: "unexpected status 403".into(),
                },
                ExitCode::Unavailable,
            ),
            (
                OciIndexError::IndexHttpFailed {
                    url: "https://index.ocx.sh/c/index.json".to_string(),
                    status: Some(503),
                    source: "unexpected status 503".into(),
                },
                ExitCode::TempFail,
            ),
        ];

        for (inner, expected) in cases {
            let arc = ocx_index::error::ArcError::from(inner);
            let leader = OciIndexError::SourceWalkFailed(arc.clone());
            let waiter = OciIndexError::SingleflightFailed(singleflight::Error::Failed(
                singleflight::SharedError::for_test(OciIndexError::SourceWalkFailed(arc)),
            ));

            let leader_code = crate::exit::classify_library_error(&leader);
            let waiter_code = crate::exit::classify_library_error(&waiter);
            assert_eq!(leader_code, expected, "leader must report the source walk's own code");
            assert_eq!(
                waiter_code, leader_code,
                "waiter must report the same code as the leader for the same failure"
            );
        }
    }
}
