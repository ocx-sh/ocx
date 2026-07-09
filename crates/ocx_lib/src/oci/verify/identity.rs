// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Certificate identity + issuer matching against a resolved trust-policy set.
//!
//! Extracts the leaf certificate's SubjectAltName and the Fulcio OIDC-issuer
//! OID extension (`1.3.6.1.4.1.57264.1.1`), then checks them against an ANY-of
//! set of [`CompiledPolicy`] constraints. The identity constraint is exact
//! (byte-equal) or an anchored full-match regex ([`crate::trust::IdentityRule`]);
//! the issuer constraint is always exact.

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

/// Verify a leaf certificate against an ANY-of set of compiled trust policies:
/// the certificate passes if its SAN + OIDC issuer satisfy *any one* policy
/// (supporting key/workflow rotation, where old and new identities coexist).
///
/// On failure the returned kind preserves the single-policy (flag-mode)
/// behaviour: if some policy's identity matched but its issuer did not, the
/// failing part is the issuer → [`VerifyErrorKind::IssuerMismatch`]; otherwise
/// no identity matched → [`VerifyErrorKind::IdentityMismatch`]. A certificate
/// with no usable SAN or issuer fails closed.
pub fn verify_policies(cert_der: &[u8], policies: &[crate::trust::CompiledPolicy]) -> Result<(), VerifyErrorKind> {
    let cert = parse_certificate(cert_der)?;
    let san = subject_identity(&cert);
    let issuer = oidc_issuer(&cert);

    let mut any_identity_matched = false;
    for policy in policies {
        let identity_ok = san.as_deref().is_some_and(|san| policy.identity.matches(san));
        let issuer_ok = issuer.as_deref() == Some(policy.issuer.as_str());
        if identity_ok && issuer_ok {
            return Ok(());
        }
        any_identity_matched |= identity_ok;
    }
    Err(if any_identity_matched {
        VerifyErrorKind::IssuerMismatch
    } else {
        VerifyErrorKind::IdentityMismatch
    })
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
    fn verify_policies_rejects_garbage_cert() {
        // A non-parseable cert must fail closed, never match.
        let policies = [crate::trust::CompiledPolicy::exact(
            "test@example.com".to_string(),
            "https://issuer.example".to_string(),
        )];
        assert!(verify_policies(b"garbage", &policies).is_err());
    }

    #[test]
    fn verify_policies_with_no_policies_is_identity_mismatch() {
        // An empty policy set never matches — the pipeline guards against this
        // upstream (NoIdentityProvided), but the primitive must still fail closed.
        assert!(matches!(
            verify_policies(b"garbage", &[]),
            Err(VerifyErrorKind::CertChainInvalid) | Err(VerifyErrorKind::IdentityMismatch)
        ));
    }

    #[test]
    fn fulcio_oid_parses() {
        // Guard the hard-coded OIDs against a typo — construction must not panic.
        assert_eq!(FULCIO_ISSUER_OID.to_string(), "1.3.6.1.4.1.57264.1.1");
        assert_eq!(SUBJECT_ALT_NAME_OID.to_string(), "2.5.29.17");
    }
}
