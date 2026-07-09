// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Certificate identity + issuer matchers.
//!
//! Exact-match (byte-equal, no normalization) for `--certificate-identity`
//! against the leaf certificate's SubjectAltName and `--certificate-oidc-issuer`
//! against the Fulcio OIDC-issuer OID extension (`1.3.6.1.4.1.57264.1.1`).

use x509_cert::Certificate;
use x509_cert::der::{Decode, oid::ObjectIdentifier};
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;

use super::error::VerifyErrorKind;

/// Fulcio OIDC-issuer extension OID (`1.3.6.1.4.1.57264.1.1`).
const FULCIO_ISSUER_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.1");
/// SubjectAltName extension OID (`2.5.29.17`).
const SUBJECT_ALT_NAME_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.17");

/// Parse a DER leaf certificate, mapping failures to `CertChainInvalid`.
pub(crate) fn parse_certificate(cert_der: &[u8]) -> Result<Certificate, VerifyErrorKind> {
    Certificate::from_der(cert_der).map_err(|_| VerifyErrorKind::CertChainInvalid)
}

/// Extract the certificate's signing identity from its SubjectAltName.
///
/// Returns the first RFC822 (email) or URI general name — the two forms Fulcio
/// issues for human and workload identities. `None` when no SAN is present.
pub(crate) fn subject_identity(cert: &Certificate) -> Option<String> {
    let extensions = cert.tbs_certificate.extensions.as_deref()?;
    let ext = extensions.iter().find(|e| e.extn_id == SUBJECT_ALT_NAME_OID)?;
    let san = SubjectAltName::from_der(ext.extn_value.as_bytes()).ok()?;
    san.0.iter().find_map(|name| match name {
        GeneralName::Rfc822Name(email) => Some(email.as_str().to_owned()),
        GeneralName::UniformResourceIdentifier(uri) => Some(uri.as_str().to_owned()),
        _ => None,
    })
}

/// Extract the OIDC issuer URL from the Fulcio issuer OID extension.
///
/// The extension value is a DER `UTF8String` carrying the issuer URL. `None`
/// when the extension is absent or malformed.
pub(crate) fn oidc_issuer(cert: &Certificate) -> Option<String> {
    let extensions = cert.tbs_certificate.extensions.as_deref()?;
    let ext = extensions.iter().find(|e| e.extn_id == FULCIO_ISSUER_OID)?;
    let raw = ext.extn_value.as_bytes();
    // Fulcio encodes v1 of this extension as a bare DER UTF8String.
    x509_cert::der::asn1::Utf8StringRef::from_der(raw)
        .ok()
        .map(|s| s.as_str().to_owned())
}

/// Certificate SAN matcher (exact, byte-equal).
pub struct IdentityMatcher {
    expected: String,
}

impl IdentityMatcher {
    /// Construct a matcher for the given expected identity string.
    pub fn new(expected: String) -> Self {
        Self { expected }
    }

    /// Verify the certificate's SAN equals the expected identity byte-for-byte.
    ///
    /// Returns [`VerifyErrorKind::IdentityMismatch`] on mismatch or when the
    /// certificate carries no usable SAN.
    pub fn verify(&self, cert_der: &[u8]) -> Result<(), VerifyErrorKind> {
        let cert = parse_certificate(cert_der)?;
        match subject_identity(&cert) {
            Some(san) if san == self.expected => Ok(()),
            _ => Err(VerifyErrorKind::IdentityMismatch),
        }
    }
}

/// Certificate issuer matcher (Fulcio OIDC-issuer OID extension, exact).
pub struct IssuerMatcher {
    expected: String,
}

impl IssuerMatcher {
    /// Construct a matcher for the given expected issuer URL.
    pub fn new(expected: String) -> Self {
        Self { expected }
    }

    /// Verify the certificate's issuer extension equals the expected URL.
    ///
    /// Returns [`VerifyErrorKind::IssuerMismatch`] on mismatch or when the
    /// extension is absent.
    pub fn verify(&self, cert_der: &[u8]) -> Result<(), VerifyErrorKind> {
        let cert = parse_certificate(cert_der)?;
        match oidc_issuer(&cert) {
            Some(issuer) if issuer == self.expected => Ok(()),
            _ => Err(VerifyErrorKind::IssuerMismatch),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit smoke tests: garbage input must be rejected (never panic, never a
    //! false positive). The positive-path SAN/issuer extraction + exact-match
    //! semantics (byte-equal, trailing-slash, mixed-case) are validated
    //! end-to-end against real Fulcio-minted certs by the acceptance suite
    //! (`test/tests/test_verify.py`: identity/issuer mismatch → exit 77).
    use super::*;

    #[test]
    fn parse_rejects_non_certificate_bytes() {
        assert!(matches!(
            parse_certificate(b"not a cert"),
            Err(VerifyErrorKind::CertChainInvalid)
        ));
        assert!(matches!(parse_certificate(&[]), Err(VerifyErrorKind::CertChainInvalid)));
    }

    #[test]
    fn identity_matcher_rejects_garbage_cert() {
        let matcher = IdentityMatcher::new("test@example.com".to_string());
        // A non-parseable cert must fail closed, never match.
        assert!(matcher.verify(b"garbage").is_err());
    }

    #[test]
    fn issuer_matcher_rejects_garbage_cert() {
        let matcher = IssuerMatcher::new("https://issuer.example".to_string());
        assert!(matcher.verify(b"garbage").is_err());
    }

    #[test]
    fn fulcio_oid_parses() {
        // Guard the hard-coded OIDs against a typo — construction must not panic.
        assert_eq!(FULCIO_ISSUER_OID.to_string(), "1.3.6.1.4.1.57264.1.1");
        assert_eq!(SUBJECT_ALT_NAME_OID.to_string(), "2.5.29.17");
    }
}
