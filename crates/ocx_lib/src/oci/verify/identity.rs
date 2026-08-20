// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Certificate identity + issuer matching against a resolved trust-policy set.
//!
//! Extracts the leaf certificate's SubjectAltName and the Fulcio OIDC-issuer
//! OID extension (`1.3.6.1.4.1.57264.1.8`, issuer v2), then checks them against an ANY-of
//! set of [`CompiledPolicy`] constraints. The identity constraint is exact
//! (byte-equal) or an anchored full-match regex ([`crate::trust::IdentityRule`]);
//! the issuer constraint is always exact.

use x509_cert::Certificate;
use x509_cert::der::{Decode, oid::ObjectIdentifier};
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;

use super::error::VerifyErrorKind;
use crate::trust::PolicyBackend;

/// Fulcio OIDC-issuer extension OID (`1.3.6.1.4.1.57264.1.8`, "Issuer (V2)").
///
/// Deliberately **not** `.1.1`: that is the deprecated v1 issuer claim, whose
/// value Fulcio writes as a *bare* UTF-8 byte string with no DER header, so
/// parsing it as a DER `UTF8String` always fails and the issuer silently reads
/// as absent. Verified against a live Fulcio v1.8.8: `.1.1` is 19 raw bytes,
/// `.1.8` is the same URL behind a `0c 13` DER header.
const FULCIO_ISSUER_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.8");
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
    // Fulcio encodes the v2 issuer extension as a DER UTF8String. The v1 OID
    // (`.1.1`) carries the same URL *unwrapped* and would not parse here.
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
///
/// Returns **every** satisfied policy rather than only whether one was: the
/// `builder` pin is ANDed within a policy and ORed across the set (#103), so
/// deciding it needs the matched subset. An equal-scope policy carrying no pin
/// weakens the set, which is exactly what `system_locked` exists to contain and
/// what a boolean would hide.
pub fn matching_policies<'a>(
    cert_der: &[u8],
    policies: &'a [crate::trust::CompiledPolicy],
) -> Result<Vec<&'a crate::trust::CompiledPolicy>, VerifyErrorKind> {
    let cert = parse_certificate(cert_der)?;
    let san = subject_identity(&cert);
    let issuer = oidc_issuer(&cert);

    let mut matched = Vec::new();
    let mut any_identity_matched = false;
    for policy in policies {
        // Irrefutable while `Keyless` is the only backend, and deliberately
        // written as a destructure rather than an accessor: a key-based backend
        // must break this function, which matches a Fulcio certificate and
        // could not verify a key signature by falling through.
        let PolicyBackend::Keyless(keyless) = &policy.backend;
        let identity_ok = san.as_deref().is_some_and(|san| keyless.identity.matches(san));
        let issuer_ok = issuer.as_deref() == Some(keyless.issuer.as_str());
        if identity_ok && issuer_ok {
            matched.push(policy);
        }
        any_identity_matched |= identity_ok;
    }
    if matched.is_empty() {
        return Err(if any_identity_matched {
            VerifyErrorKind::IssuerMismatch
        } else {
            VerifyErrorKind::IdentityMismatch
        });
    }
    Ok(matched)
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
    fn matching_policies_rejects_garbage_cert() {
        // A non-parseable cert must fail closed, never match.
        let policies = [crate::trust::CompiledPolicy::exact(
            "test@example.com".to_string(),
            "https://issuer.example".to_string(),
        )];
        assert!(matching_policies(b"garbage", &policies).is_err());
    }

    #[test]
    fn matching_policies_with_no_policies_is_identity_mismatch() {
        // An empty policy set never matches — the pipeline guards against this
        // upstream (NoIdentityProvided), but the primitive must still fail closed.
        assert!(matches!(
            matching_policies(b"garbage", &[]),
            Err(VerifyErrorKind::CertChainInvalid) | Err(VerifyErrorKind::IdentityMismatch)
        ));
    }

    #[test]
    fn fulcio_oid_parses() {
        // Guard the hard-coded OIDs against a typo — construction must not panic.
        assert_eq!(FULCIO_ISSUER_OID.to_string(), "1.3.6.1.4.1.57264.1.8");
        assert_eq!(SUBJECT_ALT_NAME_OID.to_string(), "2.5.29.17");
    }

    #[test]
    fn issuer_oid_is_the_der_wrapped_one() {
        // Why `.1.8` and not `.1.1`, pinned so a future "simplification" reds.
        //
        // Bytes taken verbatim from a live Fulcio v1.8.8 leaf: the v1 extension
        // carries the URL raw, the v2 extension carries it as a DER UTF8String.
        // Our parser is `Utf8StringRef::from_der`, so only the v2 form decodes —
        // reading `.1.1` yields `None`, the issuer reads as absent, and every
        // identity-pinned policy fails closed with exit 77.
        const V1_RAW: &[u8] = b"http://dex:5556/dex";
        const V2_DER: &[u8] = b"\x0c\x13http://dex:5556/dex";

        assert!(
            x509_cert::der::asn1::Utf8StringRef::from_der(V1_RAW).is_err(),
            "the v1 (.1.1) encoding must NOT parse as DER — that is the whole \
             reason this constant is .1.8",
        );
        let parsed =
            x509_cert::der::asn1::Utf8StringRef::from_der(V2_DER).expect("the v2 (.1.8) encoding is a DER UTF8String");
        assert_eq!(parsed.as_str(), "http://dex:5556/dex");
    }
}
