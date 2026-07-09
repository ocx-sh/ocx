// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify error types (three-layer: [`VerifyError`] + [`VerifyErrorKind`]).
//!
//! Variant inventory is the ADR-canonical set per C-S1-2. Each variant maps
//! to a distinct exit code via [`ClassifyErrorKind`].

use crate::cli::{ClassifyErrorKind, ClassifyExitCode, ExitCode};
use crate::oci::Identifier;
use crate::oci::endpoint::UrlRejection;

/// Top-level verify error carrying the identifier being verified + the kind.
#[derive(Debug, thiserror::Error)]
#[error("{identifier}: {kind}")]
pub struct VerifyError {
    /// Identifier being verified when the failure occurred.
    pub identifier: Identifier,
    /// Discriminant kind of the failure.
    #[source]
    pub kind: VerifyErrorKind,
}

impl VerifyError {
    /// Build a [`VerifyError`] from an identifier + kind.
    pub fn new(identifier: Identifier, kind: VerifyErrorKind) -> Self {
        Self { identifier, kind }
    }
}

impl ClassifyExitCode for VerifyError {
    fn classify(&self) -> Option<ExitCode> {
        Some(self.kind.exit_code())
    }
}

/// Discriminant kind for [`VerifyError`].
///
/// Canonical ADR names per C-S1-2: `IdentityMismatch`, `IssuerMismatch`,
/// `BundleParseFailed`, `RekorSetInvalid`, etc.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VerifyErrorKind {
    /// No referrers found for target manifest.
    ///
    /// Exit 79 (`NotFound`). Publisher has not signed, or signed a different platform.
    #[error("no signatures found for target")]
    NoSignaturesFound,

    /// Referrer(s) found but none has a recognized Sigstore bundle artifactType.
    ///
    /// Exit 79. May be a legacy tag-based signature (Slice 2) or a non-Sigstore attestation.
    #[error("no usable Sigstore bundle among referrers")]
    NoUsableBundle,

    /// Cert SAN does not match `--certificate-identity`.
    ///
    /// Exit 77 (`PermissionDenied`).
    #[error("certificate identity mismatch")]
    IdentityMismatch,

    /// Cert issuer does not match `--certificate-oidc-issuer`.
    ///
    /// Exit 77 (`PermissionDenied`).
    #[error("certificate OIDC issuer mismatch")]
    IssuerMismatch,

    /// Cert chain does not verify against TUF root.
    ///
    /// Exit 65 (`DataError`). TUF root out of date, or cert is forged.
    #[error("certificate chain does not verify against trust root")]
    CertChainInvalid,

    /// Signature does not verify over subject digest.
    ///
    /// Exit 65 (`DataError`). Strongest possible failure — bundle contents tampered.
    #[error("signature does not verify over subject digest")]
    SignatureInvalid,

    /// Rekor SET does not verify against Rekor public key.
    ///
    /// Exit 65 (`DataError`). A cryptographically invalid SET is a data integrity
    /// failure — the bundle has been tampered with. This is distinct from
    /// [`Self::RekorUnavailable`] (service down, retry may help): no amount of
    /// retrying will fix a tampered SET, and callers must not treat this as a
    /// transient failure.
    #[error("Rekor SET does not verify")]
    RekorSetInvalid,

    /// Rekor v2 transition: bundle has no SET but has an RFC 3161 TSA timestamp.
    ///
    /// Exit 83. v1 cannot verify TSA; full Rekor v2 support deferred until
    /// sigstore-rs ships a v2 client.
    #[error("Rekor SET absent but TSA timestamp present (Rekor v2 transition)")]
    RekorSetAbsentTsaPresent,

    /// Registry returned 404 on referrers endpoint.
    ///
    /// Exit 84 (`ReferrersUnsupported`). The outer `VerifyError` Display
    /// (`"{identifier}: {kind}"`) already prefixes this with the registry
    /// host, so the message itself does not repeat it.
    #[error(
        "registry does not support the OCI Referrers API (requires OCI Distribution 1.1+); \
         supply-chain commands are unavailable for this registry"
    )]
    ReferrersUnsupported,

    /// Rekor unavailable during verify.
    ///
    /// Exit 83. Distinct from [`Self::RekorSetInvalid`] — retry is appropriate.
    #[error("Rekor transparency log unavailable")]
    RekorUnavailable,

    /// Bundle parse failed (not v0.3, corrupted JSON).
    ///
    /// Exit 65 (`DataError`).
    #[error("bundle parse failed")]
    BundleParseFailed,

    /// Trust root could not be loaded (embedded asset missing, TUF fetch failed).
    ///
    /// Exit 78 (`ConfigError`).
    #[error("trust root unavailable")]
    TrustRootUnavailable,

    /// Trust root PEM failed to load (malformed PEM, no certificate blocks,
    /// TUF fetch failed, etc.).
    ///
    /// Exit 78 (`ConfigError`). The reason is encoded as a typed discriminant
    /// (`TrustRootLoadReason`) so callers can distinguish actionable failure
    /// modes without parsing stderr.
    #[error("trust root load failed: {0}")]
    TrustRootLoad(TrustRootLoadReason),

    /// User-supplied Sigstore endpoint URL failed SSRF/scheme validation.
    ///
    /// Surfaces at the boundary where `--rekor-url` is parsed by `ocx package
    /// verify`. Exit 64 (`UsageError`) — a malformed flag value is a CLI
    /// misuse, not a runtime fault. The `endpoint` field carries the flag
    /// name (e.g. `--rekor-url`) so the envelope `error.detail` is
    /// programmatically dispatchable.
    #[error("invalid {endpoint} URL: {reason}")]
    InvalidEndpointUrl {
        /// Flag name the URL was supplied via (e.g. `--rekor-url`).
        endpoint: String,
        /// Structured rejection reason from [`crate::oci::endpoint::validate_sigstore_url`].
        #[source]
        reason: UrlRejection,
    },

    /// Catch-all for verify-side failures outside the codes above (index
    /// resolution, digest parse, malformed URL join).
    ///
    /// Exit 1 (`Failure`). Carries the underlying error via `#[source]` so
    /// `classify_error` chain-walking and `{err:#}` diagnostics preserve the
    /// cause — never erase it with `.to_string()`.
    #[error("internal verification error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Typed discriminant for [`VerifyErrorKind::TrustRootLoad`].
///
/// Each variant maps to a distinct user-facing remediation; replacing the
/// previous free-form `String reason` with this enum lets callers (and
/// integration tests) pattern-match on the failure mode without string
/// matching, and prevents accidental introduction of paths or other
/// sensitive content into the reason text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrustRootLoadReason {
    /// `TrustRoot::load_embedded` invoked but the compile-time TUF asset is
    /// not present (Slice 1: not yet shipped).
    #[error("embedded trust-root asset is not bundled in this build")]
    EmbeddedAssetMissing,

    /// I/O error reading a trust-root asset (filesystem or embedded source).
    #[error("trust-root asset read failed")]
    AssetReadFailed {
        /// Underlying I/O / source error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// TUF fetch returned a non-2xx HTTP status.
    #[error("TUF fetch failed: HTTP {status}")]
    TufFetchFailed {
        /// HTTP status code returned by the TUF endpoint.
        status: u16,
    },

    /// TUF fetch did not complete within the configured deadline.
    #[error("TUF fetch timed out")]
    TufFetchTimeout,

    /// PEM bytes parsed but did not yield a valid certificate body.
    #[error("PEM parse failed: {detail}")]
    PemParseFailed {
        /// Short detail (e.g., `"unexpected block label"`). Never embed file
        /// paths or other sensitive content here.
        detail: String,
    },

    /// PEM input contained zero `CERTIFICATE` blocks — input was structurally
    /// valid PEM but carried no certificate body.
    #[error("no certificate blocks in trust-root PEM")]
    NoCertificateBlocks,
}

impl ClassifyErrorKind for VerifyErrorKind {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::NoSignaturesFound | Self::NoUsableBundle => ExitCode::NotFound,
            Self::IdentityMismatch | Self::IssuerMismatch => ExitCode::PermissionDenied,
            Self::CertChainInvalid
            | Self::SignatureInvalid
            | Self::BundleParseFailed
            // RekorSetInvalid is a data integrity failure (tampered bundle), not a
            // service-unavailability signal. Exit 65 so retry logic does not fire.
            | Self::RekorSetInvalid => ExitCode::DataError,
            Self::RekorSetAbsentTsaPresent | Self::RekorUnavailable => ExitCode::RekorUnavailable,
            Self::ReferrersUnsupported => ExitCode::ReferrersUnsupported,
            Self::TrustRootUnavailable | Self::TrustRootLoad(_) => ExitCode::ConfigError,
            Self::InvalidEndpointUrl { .. } => ExitCode::UsageError,
            Self::Internal(_) => ExitCode::Failure,
        }
    }

    fn kind_detail(&self) -> &'static str {
        // Frozen contract C-S1-1: snake_case parallel of the variant name.
        // Exhaustive match — no wildcard, so adding a variant forces a new arm.
        match self {
            Self::NoSignaturesFound => "no_signatures_found",
            Self::NoUsableBundle => "no_usable_bundle",
            Self::IdentityMismatch => "identity_mismatch",
            Self::IssuerMismatch => "issuer_mismatch",
            Self::CertChainInvalid => "cert_chain_invalid",
            Self::SignatureInvalid => "signature_invalid",
            Self::RekorSetInvalid => "rekor_set_invalid",
            Self::RekorSetAbsentTsaPresent => "rekor_set_absent_tsa_present",
            Self::ReferrersUnsupported => "referrers_unsupported",
            Self::RekorUnavailable => "rekor_unavailable",
            Self::BundleParseFailed => "bundle_parse_failed",
            Self::TrustRootUnavailable => "trust_root_unavailable",
            Self::TrustRootLoad(_) => "trust_root_load",
            Self::InvalidEndpointUrl { .. } => "invalid_endpoint_url",
            Self::Internal(_) => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    //! ADR §C-S1-2 canonical VerifyErrorKind contract tests.
    //!
    //! Variant names and their exit-code mappings are frozen — consumers switch
    //! on `$?` (79 = not signed, 77 = wrong signer, 83 = Rekor down, 84 = no
    //! referrers API). Any change to these tests is a user-visible contract
    //! change.
    use super::*;

    fn id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    #[test]
    fn not_found_family_maps_to_not_found_exit() {
        // "not signed" signal — publisher never signed or signed a different platform.
        for kind in [VerifyErrorKind::NoSignaturesFound, VerifyErrorKind::NoUsableBundle] {
            assert_eq!(kind.exit_code(), ExitCode::NotFound, "variant: {kind:?}");
        }
    }

    #[test]
    fn identity_family_maps_to_permission_denied() {
        // 77 = "you verified, but not by the signer you expected".
        assert_eq!(
            VerifyErrorKind::IdentityMismatch.exit_code(),
            ExitCode::PermissionDenied
        );
        assert_eq!(VerifyErrorKind::IssuerMismatch.exit_code(), ExitCode::PermissionDenied);
    }

    #[test]
    fn data_error_family_maps_to_data_error() {
        // 65 = "something in the bundle doesn't verify or doesn't parse".
        for kind in [
            VerifyErrorKind::CertChainInvalid,
            VerifyErrorKind::SignatureInvalid,
            VerifyErrorKind::BundleParseFailed,
        ] {
            assert_eq!(kind.exit_code(), ExitCode::DataError, "variant: {kind:?}");
        }
    }

    #[test]
    fn rekor_set_invalid_maps_to_data_error() {
        // RekorSetInvalid is a tampered-bundle / crypto failure — exit 65 (DataError),
        // NOT exit 83 (RekorUnavailable). A `case $? in 83) retry` handler must not
        // retry a tampered SET.
        assert_eq!(VerifyErrorKind::RekorSetInvalid.exit_code(), ExitCode::DataError);
    }

    #[test]
    fn rekor_unavailable_family_maps_to_rekor_unavailable() {
        // 83 = "Rekor service unreachable or TSA transition" — retry may help.
        for kind in [
            VerifyErrorKind::RekorSetAbsentTsaPresent,
            VerifyErrorKind::RekorUnavailable,
        ] {
            assert_eq!(kind.exit_code(), ExitCode::RekorUnavailable, "variant: {kind:?}");
        }
    }

    #[test]
    fn referrers_unsupported_maps_to_referrers_unsupported() {
        assert_eq!(
            VerifyErrorKind::ReferrersUnsupported.exit_code(),
            ExitCode::ReferrersUnsupported,
        );
    }

    #[test]
    fn trust_root_unavailable_maps_to_config_error() {
        assert_eq!(VerifyErrorKind::TrustRootUnavailable.exit_code(), ExitCode::ConfigError);
    }

    #[test]
    fn verify_error_display_prefixes_identifier() {
        let err = VerifyError::new(id(), VerifyErrorKind::IdentityMismatch);
        let msg = format!("{err}");
        assert!(msg.starts_with("registry.example/pkg:1.0:"), "got: {msg}");
        assert!(msg.contains("certificate identity mismatch"), "got: {msg}");
    }

    #[test]
    fn verify_error_kind_display_rules() {
        // C-GOOD-ERR: lowercase leading word, no trailing period (acronyms canonical).
        assert_eq!(
            format!("{}", VerifyErrorKind::NoSignaturesFound),
            "no signatures found for target"
        );
        assert_eq!(
            format!("{}", VerifyErrorKind::IdentityMismatch),
            "certificate identity mismatch"
        );
        assert_eq!(
            format!("{}", VerifyErrorKind::RekorUnavailable),
            "Rekor transparency log unavailable"
        );
        for kind in [
            VerifyErrorKind::NoSignaturesFound,
            VerifyErrorKind::IdentityMismatch,
            VerifyErrorKind::IssuerMismatch,
            VerifyErrorKind::CertChainInvalid,
            VerifyErrorKind::SignatureInvalid,
            VerifyErrorKind::RekorSetInvalid,
            VerifyErrorKind::ReferrersUnsupported,
            VerifyErrorKind::BundleParseFailed,
            VerifyErrorKind::TrustRootUnavailable,
        ] {
            let msg = format!("{kind}");
            assert!(!msg.ends_with('.'), "trailing period on: {msg}");
        }
    }

    #[test]
    fn verify_error_classify_delegates_to_kind() {
        let err = VerifyError::new(id(), VerifyErrorKind::IssuerMismatch);
        assert_eq!(err.classify(), Some(ExitCode::PermissionDenied));
    }

    #[test]
    fn verify_error_source_chain_exposes_kind() {
        use std::error::Error;
        let err = VerifyError::new(id(), VerifyErrorKind::BundleParseFailed);
        let source = err.source().expect("VerifyError has source");
        assert_eq!(format!("{source}"), "bundle parse failed");
    }

    #[test]
    fn invalid_endpoint_url_maps_to_usage_error() {
        use crate::oci::endpoint::UrlRejection;
        // Verify side borrows its own InvalidEndpointUrl variant so the exit-code
        // classification is independent of the sign side.
        let kind = VerifyErrorKind::InvalidEndpointUrl {
            endpoint: "--rekor-url".into(),
            reason: UrlRejection {
                reason: "URL must use HTTPS".into(),
            },
        };
        assert_eq!(kind.exit_code(), ExitCode::UsageError);
    }

    #[test]
    fn trust_root_load_maps_to_config_error() {
        // Every TrustRootLoadReason variant produces ConfigError.
        // ADR §C-S1-2: trust root failures are configuration-layer, not runtime faults.
        // Asset-read failures carry a boxed source — construct one via a synthetic
        // io::Error so the source-carrying branch is also covered.
        let asset_read_source: Box<dyn std::error::Error + Send + Sync> =
            Box::new(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "synthetic"));
        let reasons: Vec<TrustRootLoadReason> = vec![
            TrustRootLoadReason::EmbeddedAssetMissing,
            TrustRootLoadReason::AssetReadFailed {
                source: asset_read_source,
            },
            TrustRootLoadReason::TufFetchFailed { status: 503 },
            TrustRootLoadReason::TufFetchTimeout,
            TrustRootLoadReason::PemParseFailed {
                detail: "unexpected block label".into(),
            },
            TrustRootLoadReason::NoCertificateBlocks,
        ];
        for reason in reasons {
            let kind = VerifyErrorKind::TrustRootLoad(reason);
            assert_eq!(kind.exit_code(), ExitCode::ConfigError, "variant: {kind:?}");
        }
    }

    #[test]
    fn kind_detail_values_are_stable() {
        // C-S1-1 frozen contract: these strings ship in JSON envelopes and consumer
        // scripts dispatch on them. A rename or typo here is a user-visible breaking
        // change. The exhaustive match in `kind_detail()` ensures a new variant forces
        // a new arm there; this table ensures the *string value* for each arm is pinned.
        use crate::oci::endpoint::UrlRejection;
        use VerifyErrorKind::*;

        // Construct one representative instance per variant.
        // `TrustRootLoad` carries a `TrustRootLoadReason`; use the simplest variant.
        // `InvalidEndpointUrl` carries a `UrlRejection` borrowed from the sign module.
        let pairs: &[(&'static str, VerifyErrorKind)] = &[
            ("no_signatures_found", NoSignaturesFound),
            ("no_usable_bundle", NoUsableBundle),
            ("identity_mismatch", IdentityMismatch),
            ("issuer_mismatch", IssuerMismatch),
            ("cert_chain_invalid", CertChainInvalid),
            ("signature_invalid", SignatureInvalid),
            ("rekor_set_invalid", RekorSetInvalid),
            ("rekor_set_absent_tsa_present", RekorSetAbsentTsaPresent),
            ("referrers_unsupported", ReferrersUnsupported),
            ("rekor_unavailable", RekorUnavailable),
            ("bundle_parse_failed", BundleParseFailed),
            ("trust_root_unavailable", TrustRootUnavailable),
            (
                "trust_root_load",
                TrustRootLoad(TrustRootLoadReason::EmbeddedAssetMissing),
            ),
            (
                "invalid_endpoint_url",
                InvalidEndpointUrl {
                    endpoint: "--rekor-url".into(),
                    reason: UrlRejection {
                        reason: "URL must use HTTPS".into(),
                    },
                },
            ),
        ];

        for (expected, kind) in pairs {
            assert_eq!(kind.kind_detail(), *expected, "kind_detail() drift for {kind:?}",);
        }
    }
}
