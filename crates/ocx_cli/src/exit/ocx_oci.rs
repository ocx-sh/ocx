// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the `ocx_oci` family. Its types declare their own codes with `#[derive(Classify)]`.

use ocx_oci::auth::error::AuthError;
use ocx_oci::client::error::ClientError;
use ocx_oci::endpoint::UrlRejection;
use ocx_oci::platform::error::PlatformError;
use ocx_oci::ssrf::SsrfError;

#[cfg(test)]
mod tests {
    use super::*;
    use docker_credential::CredentialRetrievalError as Helper;
    use ocx_exit::{ClassifyExitCode, ExitCode};
    use ocx_oci::endpoint::Url;
    use ocx_oci::endpoint::validate_sigstore_url;
    use std::net::IpAddr;

    use std::path::PathBuf;

    // ── moved from ocx_oci::auth::error with the impl ──

    fn ec(e: AuthError) -> Option<ExitCode> {
        e.classify()
    }

    #[test]
    fn write_config_failed_classifies_to_io_error() {
        let e = AuthError::WriteConfigFailed {
            path: PathBuf::from("/tmp/x"),
            source: std::io::Error::other("e"),
        };
        assert_eq!(ec(e), Some(ExitCode::IoError));
    }

    #[test]
    fn no_credential_store_available_classifies_to_config_error() {
        let e = AuthError::NoCredentialStoreAvailable;
        assert_eq!(ec(e), Some(ExitCode::ConfigError));
    }

    #[test]
    fn login_rejected_classifies_to_auth_error() {
        let e = AuthError::LoginRejected {
            registry: "ghcr.io".into(),
        };
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    /// A probe that never completed must NOT read as 80. Asserting two
    /// different wrapped failures is what pins the delegation — a single case
    /// passes just as well against a hardcoded constant — and asserting
    /// `!= AuthError` is what pins the split from `LoginRejected`, which is the
    /// bug: exit 80 routes a CI script into "refresh credentials", and the
    /// retry with a fresh credential fails identically forever.
    #[test]
    fn a_failed_probe_delegates_its_exit_code_and_never_reads_as_rejected_credentials() {
        use ocx_oci::client::error::ClientError;

        let probe = |source: ClientError| AuthError::ProbeFailed {
            registry: "localhost:5000".into(),
            source: Box::new(source),
        };

        assert_eq!(
            ec(probe(ClientError::RegistryTransient(Box::new(std::io::Error::other(
                "connect refused"
            ))))),
            Some(ExitCode::TempFail),
        );
        assert_eq!(
            ec(probe(ClientError::UnsafeDestination(Box::new(std::io::Error::other(
                "plaintext realm"
            ))))),
            Some(ExitCode::DataError),
        );
        assert_ne!(
            ec(probe(ClientError::RegistryTransient(Box::new(std::io::Error::other(
                "connect refused"
            ))))),
            Some(ExitCode::AuthError),
            "the credential was never judged, so this must not classify as an auth failure"
        );
    }

    // ── moved from ocx_oci::client with the impl ──

    // ── moved from ocx_oci::client::error with the impl ──

    /// The three members of the `DataError` alternation whose own assertions
    /// WP-10 dropped: a response that is not a manifest, an image index that
    /// violates the spec, and a graph past the traversal cap.
    ///
    /// Recovered from the base impl at
    /// `7adaea62:crates/ocx_lib/src/oci/client/error.rs:335`, not from the arm
    /// below. Their three tests kept their names —
    /// `an_html_manifest_response_classifies_as_a_data_error` among them — and
    /// lost the `classify_error` line each one was named after.
    ///
    /// The arm they share is still guarded by four siblings, so a whole-arm
    /// mutation reds without this; what it does not catch is one of these three
    /// being split out of the alternation, which is what these assert.
    #[test]
    fn registry_supplied_malformed_content_classifies_as_a_data_error() {
        let not_a_manifest = ClientError::NotAManifest("<html>404 Not Found</html>".into());
        assert_eq!(
            not_a_manifest.classify(),
            Some(ExitCode::DataError),
            "an HTML body where a manifest was expected is registry-supplied bad data (65)"
        );

        let over_cap = ClientError::TraversalLimitExceeded {
            limit_kind: ocx_oci::client::error::TraversalLimit::BlobsPerManifest,
            limit: 64,
            actual: 65,
            subject: "ghcr.io/owner/tool:1.0".to_string(),
        };
        assert_eq!(
            over_cap.classify(),
            Some(ExitCode::DataError),
            "a graph too large to traverse is input this build refuses, not a fault a rerun clears"
        );

        // Built through the production validator: `InvalidImageIndex`'s field
        // is private, and going through `validate_image_index` also pins that
        // the `#[from]` conversion still lands on the same arm.
        let index: ocx_oci::ImageIndex =
            serde_json::from_str(r#"{"schemaVersion":1,"manifests":[]}"#).expect("the shape parses");
        let invalid = ocx_oci::manifest::validate_image_index(&index)
            .expect_err("schemaVersion 1 violates the image-index invariant");
        assert_eq!(ClientError::from(invalid).classify(), Some(ExitCode::DataError));
    }

    /// An incomplete delivery exits 75 (retry), the registry serving wrong
    /// content exits 65 (terminal). Asserting both pins the *distinction* —
    /// folding `ShortBlobRead` into the `DataError` bucket would make a
    /// transient truncation look like a supply-chain failure to `case $?`.
    /// Mirror routing is provenance, so the exit code must be whatever the
    /// wrapped failure would have exited with on its own. Asserting two
    /// different codes is what pins the delegation — a single case passes just
    /// as well against a hardcoded constant.
    #[test]
    fn a_mirrored_failure_delegates_its_exit_code_to_the_wrapped_one() {
        let mirrored = |source: ClientError| ClientError::Mirrored {
            origin: "ghcr.io".to_string(),
            mirror: "artifactory.example.com".to_string(),
            physical: "artifactory.example.com/ghcr-remote/owner/tool:1.0".to_string(),
            source: Box::new(source),
        };

        assert_eq!(
            mirrored(ClientError::DigestMismatch {
                expected: "sha256:aaa".to_string(),
                actual: "sha256:bbb".to_string(),
            })
            .classify(),
            Some(ExitCode::DataError)
        );
        assert_eq!(
            mirrored(ClientError::RegistryTransient(Box::new(std::io::Error::other(
                "connect timed out"
            ))))
            .classify(),
            Some(ExitCode::TempFail)
        );
    }

    /// A registry that will not delete tags is an operator's fix, so it gets its own code: 69 and 75
    /// both read as "retry", and a retry never helps here. Reached through the library-error chain
    /// walker as well, the way a raised error arrives.
    #[test]
    fn a_registry_that_will_not_delete_tags_exits_82() {
        let error = ClientError::DeleteUnsupported {
            registry: "registry.test".to_string(),
            status: 405,
        };
        assert_eq!(error.classify(), Some(ExitCode::Unsupported));
        assert_eq!(
            crate::exit::classify_library_error(&error as &(dyn std::error::Error + 'static)),
            ExitCode::Unsupported
        );
        crate::exit::tests::assert_detail(&error, "registry_delete_unsupported");
    }

    #[test]
    fn a_tag_outside_the_oci_grammar_is_a_usage_error() {
        let error = ClientError::InvalidTag("registry.test/owner/tool:a/../b".to_string());
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
        assert_eq!(
            crate::exit::classify_library_error(&error as &(dyn std::error::Error + 'static)),
            ExitCode::UsageError
        );
    }

    #[test]
    fn a_tag_delete_without_exactly_one_tag_is_a_usage_error() {
        let error = ClientError::DeleteNeedsTag("registry.test/owner/tool".to_string());
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
        assert_eq!(
            crate::exit::classify_library_error(&error as &(dyn std::error::Error + 'static)),
            ExitCode::UsageError
        );
    }

    /// The refusal keeps its code behind mirror routing, like every other client failure.
    #[test]
    fn a_mirrored_delete_refusal_still_exits_82() {
        let mirrored = ClientError::Mirrored {
            origin: "ghcr.io".to_string(),
            mirror: "artifactory.example.com".to_string(),
            physical: "artifactory.example.com/ghcr-remote/owner/tool:1.0".to_string(),
            source: Box::new(ClientError::DeleteUnsupported {
                registry: "ghcr.io".to_string(),
                status: 405,
            }),
        };
        assert_eq!(mirrored.classify(), Some(ExitCode::Unsupported));
    }

    #[test]
    fn short_blob_read_is_temp_fail_while_digest_mismatch_stays_data_error() {
        assert_eq!(
            ClientError::ShortBlobRead {
                expected: 1024,
                actual: 512,
            }
            .classify(),
            Some(ExitCode::TempFail)
        );
        assert_eq!(
            ClientError::DigestMismatch {
                expected: "sha256:aaa".to_string(),
                actual: "sha256:bbb".to_string(),
            }
            .classify(),
            Some(ExitCode::DataError)
        );
    }

    /// The generic `Internal` carrier must delegate to its boxed `#[source]`,
    /// not collapse to `Failure`. This is the install/`pull_layer` route's
    /// exit-code contract: a registry-served hostile layer's traversal refusal
    /// reaches the classifier wrapped as
    /// `ClientError::internal(archive::Error::SymlinkEscape)` (see
    /// `client.rs::pull_layer_with_caps`), so a hardcoded `Failure` here made
    /// `ocx package install` exit 1 where the `--extract` route (which never
    /// crosses `ClientError`) exited 65. Asserting BOTH a classifiable inner
    /// (`DataError`) AND an opaque inner (`Failure`) pins the delegation — a
    /// hardcoded constant fails one arm or the other.
    #[test]
    fn internal_delegates_its_exit_code_to_the_wrapped_source() {
        // The bare archive refusal, exactly as `pull_layer_with_caps` now hands
        // it over: E1 made the archive tier raise its own error, so the wide
        // `ocx_lib::Error::Archive` wrapper is no longer on this route and a
        // test that still built one would stop pinning the real chain.
        let traversal = ocx_util::archive::Error::SymlinkEscape {
            link: std::path::PathBuf::from("escape"),
            target: std::path::PathBuf::from("../../../../etc"),
        };
        let wrapped = ClientError::internal(traversal);
        // The arm renders no verdict of its own — it defers to the source chain,
        // so `classify` alone is `None` (the `Mirrored` doctrine). A future change
        // back to a terminal `Some(Failure)` reddens both asserts below.
        assert_eq!(wrapped.classify(), None, "Internal must defer, not render a verdict");
        // ...and the chain walker recovers the inner archive refusal's own code:
        // a hostile layer wrapped as `ClientError::internal(SymlinkEscape)` must
        // still exit 65 — the bug that made `ocx package install` exit 1.
        assert_eq!(
            crate::exit::classify_library_error(&wrapped),
            ExitCode::DataError,
            "a traversal refusal wrapped as Internal must still exit 65"
        );
        // An unclassifiable inner still falls through to Failure.
        assert_eq!(
            crate::exit::classify_library_error(&ClientError::internal(std::io::Error::other("opaque cause"))),
            ExitCode::Failure,
            "an unclassifiable inner still falls through to Failure"
        );
    }

    // ── moved from ocx_oci::client::native_transport with the impl ──

    // ── moved from ocx_oci::client::transport with the impl ──

    // ── moved from ocx_oci::copy with the impl ──

    // ── moved from ocx_oci::endpoint with the impl ──

    // ── moved from ocx_oci::ssrf with the impl ──

    #[test]
    fn resolution_failure_classifies_to_temp_fail() {
        let error = SsrfError::Resolution {
            host: "registry.invalid".to_string(),
            source: std::io::Error::other("dns lookup failed"),
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }

    // ── moved from ocx_oci::auth::error with the impl ──

    #[test]
    fn helper_helper_communication_error_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::HelperCommunicationError);
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    #[test]
    fn helper_helper_failure_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::HelperFailure {
            helper: "test".into(),
            stdout: "".into(),
            stderr: "boom".into(),
        });
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    #[test]
    fn helper_not_on_path_classifies_to_config_error() {
        let e = AuthError::Helper(Helper::NotOnPath { name: "x".into() });
        assert_eq!(ec(e), Some(ExitCode::ConfigError));
    }

    #[test]
    fn helper_other_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::HelperCommunicationError);
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    #[test]
    fn helper_output_too_large_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::OutputTooLarge { cap_bytes: 65536 });
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    #[test]
    fn helper_timeout_classifies_to_temp_fail() {
        let e = AuthError::Helper(Helper::Timeout { seconds: 30 });
        assert_eq!(ec(e), Some(ExitCode::TempFail));
    }

    #[test]
    fn helper_unsafe_path_classifies_to_config_error() {
        let e = AuthError::Helper(Helper::UnsafePath {
            name: "x".into(),
            path: PathBuf::from("/tmp/x"),
        });
        assert_eq!(ec(e), Some(ExitCode::ConfigError));
    }

    // ── moved from ocx_oci::endpoint with the impl ──

    /// A Sigstore endpoint that does not resolve is unavailable (69), not a
    /// usage error (64).
    ///
    /// 64 tells the operator their `--fulcio-url` is malformed. A name that
    /// fails to resolve is the same class of failure as any unreachable
    /// service -- the flag was fine, the network was not. The other two rows
    /// are the paired positives: a refused address and a bad scheme are
    /// genuine usage errors and keep 64.
    ///
    /// Discriminates: build the rejection from a fixed `UsageError` and the
    /// resolution row reds.
    #[test]
    fn a_url_rejection_carrying_a_resolution_failure_classifies_as_unavailable() {
        use ocx_exit::ExitCode;

        let unresolvable = UrlRejection::from(ocx_oci::ssrf::SsrfError::Resolution {
            host: "fulcio.invalid".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "failed to lookup address information"),
        });
        assert_eq!(
            unresolvable.exit_code(),
            ExitCode::Unavailable,
            "an endpoint that does not resolve is unavailable, not a usage error"
        );
        assert_eq!(
            unresolvable.classify(),
            Some(ExitCode::Unavailable),
            "a bare rejection classifies to the verdict it carries"
        );

        let forbidden = UrlRejection::from(ocx_oci::ssrf::SsrfError::ForbiddenTarget {
            host: "fulcio.corp.test".to_string(),
            ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 1, 2, 3)),
        });
        assert_eq!(
            forbidden.exit_code(),
            ExitCode::UsageError,
            "a refused endpoint stays the 64 the sign and verify wraps already report"
        );

        let bad_scheme = unwrap_err(validate_sigstore_url("ftp://example.com/bundle", "--rekor-url"));
        assert_eq!(
            bad_scheme.exit_code(),
            ExitCode::UsageError,
            "a string-level rejection is a usage error"
        );
    }

    // ── moved from ocx_oci::ssrf with the impl ──

    #[test]
    fn forbidden_target_classifies_to_config_error() {
        let error = SsrfError::ForbiddenTarget {
            host: "169.254.169.254".to_string(),
            ip: ip("169.254.169.254"),
        };
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
    }

    /// Reds on: an OCI slug, delegated, walked or read off a rejection's verdict, naming another
    /// cause than the exit code does.
    #[test]
    fn oci_details_name_the_cause_that_decides_the_code() {
        use crate::exit::tests::assert_detail;

        let transient = || ClientError::RegistryTransient(Box::new(std::io::Error::other("connect refused")));
        assert_detail(&transient(), "registry_transient");
        let mirrored = ClientError::Mirrored {
            origin: "ghcr.io".to_string(),
            mirror: "artifactory.example.com".to_string(),
            physical: "artifactory.example.com/ghcr-remote/owner/tool:1.0".to_string(),
            source: Box::new(transient()),
        };
        assert_detail(&mirrored, "registry_transient");
        let escape = ocx_util::archive::Error::SymlinkEscape {
            link: PathBuf::from("escape"),
            target: PathBuf::from("../../../../etc"),
        };
        assert_detail(&ClientError::internal(escape), "archive_symlink_escape");
        assert_detail(
            &ClientError::internal(std::io::Error::other("opaque cause")),
            "client_internal",
        );
        let probe = AuthError::ProbeFailed {
            registry: "localhost:5000".into(),
            source: Box::new(transient()),
        };
        assert_detail(&probe, "registry_transient");
        let unresolvable = || SsrfError::Resolution {
            host: "fulcio.invalid".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "failed to lookup address information"),
        };
        assert_detail(&unresolvable(), "host_resolution_failed");
        assert_detail(&UrlRejection::from(unresolvable()), "endpoint_unresolvable");
        let forbidden = || SsrfError::ForbiddenTarget {
            host: "169.254.169.254".to_string(),
            ip: ip("169.254.169.254"),
        };
        assert_detail(&forbidden(), "ssrf_forbidden_target");
        assert_detail(&UrlRejection::from(forbidden()), "invalid_endpoint_url");
        let bad_scheme = unwrap_err(validate_sigstore_url("ftp://example.com/bundle", "--rekor-url"));
        assert_detail(&bad_scheme, "invalid_endpoint_url");
    }

    // fixture from ocx_lib (oci/endpoint)
    fn unwrap_err(result: Result<Url, UrlRejection>) -> UrlRejection {
        result.expect_err("expected validation failure")
    }

    // fixture from ocx_lib (oci/ssrf)
    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test literal is a valid IP")
    }

    // recovered from crate::exit::classify
    #[test]
    fn platform_error_maps_to_data_error() {
        // Plan taxonomy: PlatformError → DataError (65).
        //
        // Built by the real parser rather than a struct literal: `PlatformError`
        // is `#[non_exhaustive]`, so a literal cannot be written outside the
        // crate that declares it. Going through `from_str` is the stronger
        // shape anyway - it is the production path that mints the value.
        let err: PlatformError = "bad/os/arch".parse::<ocx_oci::Platform>().unwrap_err();
        assert_eq!(
            crate::exit::classify_library_error(&err as &(dyn std::error::Error + 'static)),
            ExitCode::DataError
        );
    }
}
