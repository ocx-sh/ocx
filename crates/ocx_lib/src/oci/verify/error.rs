// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify error types (three-layer: [`VerifyError`] + [`VerifyErrorKind`]).
//!
//! Variant inventory is the ADR-canonical set per C-S1-2. Each variant maps
//! to a distinct exit code via [`ClassifyErrorKind`].

use crate::cli::{ClassifyErrorKind, ClassifyExitCode, ExitCode};
use crate::oci::Identifier;

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
    /// Exit 82 (`RekorUnavailable`).
    #[error("Rekor SET does not verify")]
    RekorSetInvalid,

    /// Rekor v2 transition: bundle has no SET but has an RFC 3161 TSA timestamp.
    ///
    /// Exit 82. v1 cannot verify TSA; full Rekor v2 support deferred until
    /// sigstore-rs ships a v2 client.
    #[error("Rekor SET absent but TSA timestamp present (Rekor v2 transition)")]
    RekorSetAbsentTsaPresent,

    /// Registry returned 404 on referrers endpoint.
    ///
    /// Exit 83 (`ReferrersUnsupported`).
    #[error("registry does not support the OCI Referrers API")]
    ReferrersUnsupported,

    /// Rekor unavailable during verify.
    ///
    /// Exit 82. Distinct from [`Self::RekorSetInvalid`] — retry is appropriate.
    #[error("Rekor transparency log unavailable")]
    RekorUnavailable,

    /// Bundle parse failed (not v0.3, corrupted JSON).
    ///
    /// Exit 65 (`DataError`).
    #[error("bundle parse failed")]
    BundleParseFailed,

    /// Rekor Rekor lookup failed (HTTP 4xx/5xx from Rekor other than 200).
    ///
    /// Exit 82.
    #[error("Rekor lookup failed")]
    RekorLookupFailed,

    /// Certificate has expired and no valid Rekor SET is available to witness pre-expiry signing.
    ///
    /// Exit 65. Note: expired cert + valid SET is the success path (tlog witnesses
    /// that signing happened pre-expiry); this variant fires only when expiry
    /// cannot be reconciled.
    #[error("certificate expired and no valid Rekor SET witnesses pre-expiry signing")]
    CertificateExpired,

    /// Certificate has been revoked per Fulcio's CRL.
    ///
    /// Exit 65.
    #[error("certificate revoked")]
    CertificateRevoked,

    /// Trust root could not be loaded (embedded asset missing, TUF fetch failed).
    ///
    /// Exit 78 (`ConfigError`).
    #[error("trust root unavailable")]
    TrustRootUnavailable,

    /// Bundle referenced but not found in the registry (404 on blob).
    ///
    /// Exit 79 (`NotFound`).
    #[error("bundle blob not found in registry")]
    BundleNotFound,
}

impl ClassifyErrorKind for VerifyErrorKind {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::NoSignaturesFound | Self::NoUsableBundle | Self::BundleNotFound => ExitCode::NotFound,
            Self::IdentityMismatch | Self::IssuerMismatch => ExitCode::PermissionDenied,
            Self::CertChainInvalid
            | Self::SignatureInvalid
            | Self::BundleParseFailed
            | Self::CertificateExpired
            | Self::CertificateRevoked => ExitCode::DataError,
            Self::RekorSetInvalid
            | Self::RekorSetAbsentTsaPresent
            | Self::RekorUnavailable
            | Self::RekorLookupFailed => ExitCode::RekorUnavailable,
            Self::ReferrersUnsupported => ExitCode::ReferrersUnsupported,
            Self::TrustRootUnavailable => ExitCode::ConfigError,
        }
    }
}
