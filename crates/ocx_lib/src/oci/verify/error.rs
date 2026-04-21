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
    /// Exit 65 (`DataError`). A cryptographically invalid SET is a data integrity
    /// failure — the bundle has been tampered with. This is distinct from
    /// [`Self::RekorUnavailable`] (service down, retry may help): no amount of
    /// retrying will fix a tampered SET, and callers must not treat this as a
    /// transient failure.
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

    /// Trust root could not be loaded (embedded asset missing, TUF fetch failed).
    ///
    /// Exit 78 (`ConfigError`).
    #[error("trust root unavailable")]
    TrustRootUnavailable,

    /// Trust root PEM failed to parse (malformed PEM, no certificate blocks, bad UTF-8).
    ///
    /// Exit 78 (`ConfigError`).
    #[error("trust root load failed: {reason}")]
    TrustRootLoad { reason: String },
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
            Self::TrustRootUnavailable | Self::TrustRootLoad { .. } => ExitCode::ConfigError,
        }
    }
}

#[cfg(test)]
mod tests {
    //! ADR §C-S1-2 canonical VerifyErrorKind contract tests.
    //!
    //! Variant names and their exit-code mappings are frozen — consumers switch
    //! on `$?` (79 = not signed, 77 = wrong signer, 82 = Rekor down, 83 = no
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
        // NOT exit 82 (RekorUnavailable). A `case $? in 82) retry` handler must not
        // retry a tampered SET.
        assert_eq!(VerifyErrorKind::RekorSetInvalid.exit_code(), ExitCode::DataError);
    }

    #[test]
    fn rekor_unavailable_family_maps_to_rekor_unavailable() {
        // 82 = "Rekor service unreachable or TSA transition" — retry may help.
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
}
