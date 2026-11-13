// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Certificate identity + issuer matching against an ANY-of set of [`CompiledPolicy`] constraints.
//!
//! The identity matches per [`ocx_trust::IdentityRule`]; the issuer always matches exactly.

use x509_cert::Certificate;
use x509_cert::der::{Decode, oid::ObjectIdentifier};
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;

use super::error::VerifyErrorKind;
use ocx_trust::PolicyBackend;

/// Fulcio OIDC-issuer extension OID, "Issuer (V2)".
// Not v1's `.1.1`: Fulcio writes that one without a DER header, so the issuer would silently read as absent.
const FULCIO_ISSUER_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.8");
const FULCIO_ISSUER_V1_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.1");
const SUBJECT_ALT_NAME_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.17");

/// Parse a DER leaf certificate, mapping failures to `CertChainInvalid`.
pub(crate) fn parse_certificate(cert_der: &[u8]) -> Result<Certificate, VerifyErrorKind> {
    Certificate::from_der(cert_der).map_err(|_| VerifyErrorKind::CertChainInvalid)
}

/// The value of the extension `oid`, only when the certificate carries it exactly once.
///
/// RFC 5280 forbids duplicates; reading the first of two would let a crafted certificate show a verifier one value
/// and a relying party another.
fn unique_extension<'a>(cert: &'a Certificate, oid: &ObjectIdentifier) -> Option<&'a [u8]> {
    let extensions = cert.tbs_certificate.extensions.as_deref()?;
    let mut matching = extensions.iter().filter(|e| e.extn_id == *oid);
    let found = matching.next()?;
    matching.next().is_none().then(|| found.extn_value.as_bytes())
}

/// The first RFC822 (email) or URI SubjectAltName, the two forms Fulcio issues; `None` without exactly one SAN extension.
pub(crate) fn subject_identity(cert: &Certificate) -> Option<String> {
    let san = SubjectAltName::from_der(unique_extension(cert, &SUBJECT_ALT_NAME_OID)?).ok()?;
    san.0.iter().find_map(|name| match name {
        GeneralName::Rfc822Name(email) => Some(email.as_str().to_owned()),
        GeneralName::UniformResourceIdentifier(uri) => Some(uri.as_str().to_owned()),
        _ => None,
    })
}

/// The OIDC issuer URL from the Fulcio issuer extension; `None` when absent, duplicated or malformed.
pub(crate) fn oidc_issuer(cert: &Certificate) -> Option<String> {
    decode_der_utf8(unique_extension(cert, &FULCIO_ISSUER_OID)?)
}

/// A minimal-length DER UTF8String with no trailing bytes. `Utf8StringRef::from_der` refuses a non-UTF8String tag,
/// a non-minimal length and trailing data, which is what makes each of those a non-match here.
fn decode_der_utf8(raw: &[u8]) -> Option<String> {
    x509_cert::der::asn1::Utf8StringRef::from_der(raw)
        .ok()
        .map(|s| s.as_str().to_owned())
}

/// Whether every compiled claim matches the leaf certificate: the extension is present exactly once and its
/// value, decoded per the claim's encoding, equals the expected string byte for byte.
pub(crate) fn claims_match(cert: &Certificate, claims: &[ocx_trust::CompiledClaim]) -> bool {
    claims.iter().all(|claim| {
        let Ok(oid) = ObjectIdentifier::new(&claim.selector.oid) else {
            return false;
        };
        let Some(value) = unique_extension(cert, &oid) else {
            return false;
        };
        let decoded = match claim.selector.encoding {
            ocx_trust::ClaimEncoding::Raw => std::str::from_utf8(value).ok().map(str::to_owned),
            ocx_trust::ClaimEncoding::Der => decode_der_utf8(value),
        };
        decoded.is_some_and(|text| text == claim.expected)
    })
}

/// Every policy in the ANY-of set whose identity and issuer the leaf certificate satisfies.
///
/// Returns the matched subset, not a boolean, because the `builder` pin is ORed across it.
///
/// # Errors
///
/// [`VerifyErrorKind::IssuerMismatch`] when an identity matched under the wrong issuer, otherwise
/// [`VerifyErrorKind::IdentityMismatch`]; a certificate with no usable SAN or issuer fails closed.
pub fn matching_policies<'a>(
    cert_der: &[u8],
    policies: &'a [ocx_trust::CompiledPolicy],
) -> Result<Vec<&'a ocx_trust::CompiledPolicy>, VerifyErrorKind> {
    let cert = parse_certificate(cert_der)?;
    let san = subject_identity(&cert);
    let issuer = oidc_issuer(&cert);
    if issuer.is_none() && unique_extension(&cert, &FULCIO_ISSUER_V1_OID).is_some() {
        tracing::warn!(
            "certificate carries only the deprecated issuer extension 1.3.6.1.4.1.57264.1.1; the issuer is read from 1.3.6.1.4.1.57264.1.8, so no policy can match it"
        );
    }

    let mut matched = Vec::new();
    let mut any_identity_matched = false;
    for policy in policies {
        // Exhaustive, no `_`: a third `PolicyBackend` variant must break the build, not skip as "not keyless".
        let mut policy_matched = false;
        for backend in &policy.backends {
            let keyless = match backend {
                PolicyBackend::Keyless(keyless) => keyless,
                PolicyBackend::Key(_) => continue,
            };
            let identity_ok = match keyless.identity.as_ref() {
                Some(rule) => san.as_deref().is_some_and(|san| rule.matches(san)),
                None => true,
            };
            any_identity_matched |= identity_ok;
            let claims_ok = keyless.claims.is_empty() || claims_match(&cert, &keyless.claims);
            policy_matched |= identity_ok && claims_ok && issuer.as_deref() == Some(keyless.issuer.as_str());
        }
        if policy_matched {
            matched.push(policy);
        }
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

/// Every policy in the ANY-of set whose pinned public key produced `signature`; the key-mode twin of
/// [`matching_policies`].
///
/// `message` is the signed bytes, never their base64 text: the DSSE PAE ([`crate::attest::dsse::pae`]) on the
/// bundle path, the raw simplesigning payload as served on the sidecar path.
///
/// # Errors
///
/// [`VerifyErrorKind::SignatureInvalid`] when a policy key was tried and none verified;
/// [`VerifyErrorKind::IdentityMismatch`] when the policy set names no key.
pub(crate) fn matching_key_policies<'a>(
    message: &[u8],
    signature: &[u8],
    policies: &'a [ocx_trust::CompiledPolicy],
) -> Result<Vec<&'a ocx_trust::CompiledPolicy>, VerifyErrorKind> {
    let mut matched = Vec::new();
    let mut any_key_tried = false;
    for policy in policies {
        // Exhaustive, no `_`: a third `PolicyBackend` variant must break the build, not skip as "not a key".
        let mut policy_matched = false;
        for backend in &policy.backends {
            let key = match backend {
                PolicyBackend::Key(key) => key,
                PolicyBackend::Keyless(_) => continue,
            };
            any_key_tried = true;
            policy_matched |= key
                .verify_signature(sigstore::crypto::Signature::Raw(signature), message)
                .is_ok();
        }
        if policy_matched {
            matched.push(policy);
        }
    }
    if matched.is_empty() {
        return Err(if any_key_tried {
            // Untrusted key and tampered signature are indistinguishable here; both stay exit 65, which scripts rely
            // on.
            VerifyErrorKind::SignatureInvalid
        } else {
            // No key was tried, so claiming an invalid signature would assert corruption nobody observed.
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
        let policies = [ocx_trust::CompiledPolicy::exact(
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

    // ── Backend sets (WP9a) ──────────────────────────────────────────────────

    /// The real Fulcio leaf G0 captured: SAN `ocx-test@example.com`, issuer
    /// `http://dex:5556/dex`. Used rather than a synthesized certificate so the
    /// assertions below are about policy matching, not about cert construction.
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../test/tests/fixtures/golden/keyless_bundle.json");
    const GOLDEN_IDENTITY: &str = "ocx-test@example.com";
    const GOLDEN_ISSUER: &str = "http://dex:5556/dex";

    fn golden_leaf_der() -> Vec<u8> {
        use base64::Engine as _;

        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        base64::engine::general_purpose::STANDARD
            .decode(
                bundle["verificationMaterial"]["certificate"]["rawBytes"]
                    .as_str()
                    .expect("the bundle carries a leaf certificate"),
            )
            .expect("the leaf certificate is base64")
    }

    /// A compiled key backend over the golden public key — the artifact-side
    /// half a certificate can never satisfy.
    fn key_backend() -> ocx_trust::PolicyBackend {
        const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../../test/tests/fixtures/golden/keys/cosign.pub");
        ocx_trust::PolicyBackend::Key(
            sigstore::crypto::CosignVerificationKey::try_from_pem(GOLDEN_PUBLIC_KEY_PEM.as_bytes())
                .expect("cosign.pub is an SPKI PEM"),
        )
    }

    fn keyless_backend(identity: &str, issuer: &str) -> ocx_trust::PolicyBackend {
        ocx_trust::PolicyBackend::Keyless(ocx_trust::CompiledKeyless {
            identity: Some(ocx_trust::IdentityRule::Exact(identity.to_string())),
            issuer: issuer.to_string(),
            claims: Vec::new(),
        })
    }

    /// **Spec D5, falling out of the type rather than being restated.** A policy
    /// whose signers are all `kind = "key"` names nobody who signs with a
    /// certificate, so a keyless artifact must not match it — a key backend
    /// contributes nothing here and must never be read as "no objection".
    #[test]
    fn a_key_only_policy_never_matches_a_certificate() {
        let policies = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![key_backend()],
        }];
        let error = matching_policies(&golden_leaf_der(), &policies)
            .expect_err("a key signer cannot admit a keyless signature");
        assert!(matches!(error, VerifyErrorKind::IdentityMismatch), "got {error:?}");
    }

    /// The mixed policy is the migration shape, and its keyless half must still
    /// admit a keyless artifact — otherwise adding a key signer would *narrow*
    /// acceptance, which is the opposite of what an ANY-of set does.
    #[test]
    fn a_mixed_policy_matches_on_its_keyless_half() {
        let policies = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![key_backend(), keyless_backend(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
        }];
        let matched = matching_policies(&golden_leaf_der(), &policies).expect("the keyless signer matches");
        assert_eq!(matched.len(), 1);
    }

    /// Adding a signer widens: a policy whose *second* keyless entry matches is
    /// admitted, even though its first does not. A first-entry-only reading
    /// would silently drop every rotation window.
    #[test]
    fn a_later_signer_in_the_set_can_admit_the_policy() {
        let policies = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![
                keyless_backend("someone-else@example.com", GOLDEN_ISSUER),
                keyless_backend(GOLDEN_IDENTITY, GOLDEN_ISSUER),
            ],
        }];
        assert_eq!(
            matching_policies(&golden_leaf_der(), &policies)
                .expect("the second signer matches")
                .len(),
            1
        );
    }

    /// The two refusals still have to be told apart across a backend *set*: a
    /// matching identity under the wrong issuer is `IssuerMismatch`, an
    /// unmatched identity is `IdentityMismatch`. Collapsing them would lose the
    /// only signal that says "your policy is right, the issuer moved".
    #[test]
    fn identity_and_issuer_mismatches_stay_distinguishable_across_a_set() {
        let wrong_issuer = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![
                key_backend(),
                keyless_backend(GOLDEN_IDENTITY, "https://elsewhere.example"),
            ],
        }];
        assert!(matches!(
            matching_policies(&golden_leaf_der(), &wrong_issuer),
            Err(VerifyErrorKind::IssuerMismatch)
        ));

        let wrong_identity = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![keyless_backend("nobody@example.com", GOLDEN_ISSUER)],
        }];
        assert!(matches!(
            matching_policies(&golden_leaf_der(), &wrong_identity),
            Err(VerifyErrorKind::IdentityMismatch)
        ));
    }

    /// The key-mode refusal is about the **signature**, not about an identity
    /// this path never reads. A trusted key was tried and did not accept these
    /// bytes, which is exit 65 — "certificate identity mismatch" would name
    /// material a key signature does not carry, and would hide a bad signature
    /// from every caller scripting 65.
    ///
    /// Asserted beside its sibling below rather than alone: the two arms are
    /// one decision, and a test for either one on its own passes just as well
    /// against a function that always answers that arm.
    #[test]
    fn a_tried_key_that_does_not_verify_is_a_signature_failure() {
        let policies = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![key_backend()],
        }];
        let error = matching_key_policies(b"the signed message", b"not a signature", &policies)
            .expect_err("a garbage signature verifies under no key");
        assert!(matches!(error, VerifyErrorKind::SignatureInvalid), "got {error:?}");
    }

    /// Spec D5 in the key direction: a policy set naming only keyless signers
    /// never reaches `verify_signature`, so nothing was measured about the
    /// signature and calling it invalid would assert a corruption never
    /// observed. The verdict is the identity one, and it is decided on the
    /// policy set rather than on a verification outcome.
    #[test]
    fn a_keyless_only_policy_set_refuses_a_key_signature_as_an_identity_mismatch() {
        let policies = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![keyless_backend(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
        }];
        let error = matching_key_policies(b"the signed message", b"not a signature", &policies)
            .expect_err("a keyless-only policy admits no key signature");
        assert!(matches!(error, VerifyErrorKind::IdentityMismatch), "got {error:?}");
    }

    fn golden_claim(oid_arc: u32, expected: &str) -> ocx_trust::CompiledClaim {
        ocx_trust::CompiledClaim {
            selector: ocx_trust::ClaimSelector {
                oid: format!("1.3.6.1.4.1.57264.1.{oid_arc}"),
                encoding: ocx_trust::ClaimEncoding::Der,
            },
            expected: expected.to_owned(),
        }
    }

    /// green: the golden leaf's `.1.8` extension is a DER UTF8String equal to the expected value, byte for byte.
    #[test]
    fn c008_a_present_claim_with_the_exact_value_matches() {
        let cert = parse_certificate(&golden_leaf_der()).expect("the golden leaf parses");
        assert!(claims_match(&cert, &[golden_claim(8, GOLDEN_ISSUER)]));
    }

    /// red: a changed value, a prefix of the value, and an absent extension (`.1.9`, never matching even
    /// `""`) are non-matches, and every claim must match.
    #[test]
    fn c008_a_wrong_prefix_or_absent_claim_never_matches() {
        let cert = parse_certificate(&golden_leaf_der()).expect("the golden leaf parses");
        assert!(!claims_match(&cert, &[golden_claim(8, "http://dex:5556/de")]));
        assert!(!claims_match(&cert, &[golden_claim(8, "http://dex:5556/dex ")]));
        assert!(!claims_match(&cert, &[golden_claim(9, "")]));
        assert!(!claims_match(
            &cert,
            &[golden_claim(8, GOLDEN_ISSUER), golden_claim(9, "x")]
        ));
    }

    // ── claim decoding, one red and one green per rule ─────────────────

    const FULCIO_ARC: &str = "1.3.6.1.4.1.57264.1";

    fn claim(oid_suffix: u32, encoding: ocx_trust::ClaimEncoding, expected: &str) -> ocx_trust::CompiledClaim {
        ocx_trust::CompiledClaim {
            selector: ocx_trust::ClaimSelector {
                oid: format!("{FULCIO_ARC}.{oid_suffix}"),
                encoding,
            },
            expected: expected.to_owned(),
        }
    }

    /// A minimal DER UTF8String TLV, short-form length only.
    fn der_utf8(text: &str) -> Vec<u8> {
        let mut out = vec![0x0c, u8::try_from(text.len()).expect("short form")];
        out.extend_from_slice(text.as_bytes());
        out
    }

    /// A certificate carrying exactly the listed extensions. Unsigned for the chain, which `claims_match` never reads.
    fn synthesized(extensions: &[(&str, Vec<u8>)]) -> Certificate {
        use p256::ecdsa::SigningKey;
        use p256::elliptic_curve::rand_core::OsRng;
        use std::str::FromStr;
        use x509_cert::builder::{Builder, CertificateBuilder, Profile};
        use x509_cert::der::Encode;
        use x509_cert::der::asn1::OctetString;
        use x509_cert::ext::Extension;
        use x509_cert::name::Name;
        use x509_cert::serial_number::SerialNumber;
        use x509_cert::spki::SubjectPublicKeyInfoOwned;
        use x509_cert::time::Validity;

        let key = SigningKey::random(&mut OsRng);
        let spki = SubjectPublicKeyInfoOwned::from_key(*key.verifying_key()).expect("spki");
        let built = CertificateBuilder::new(
            Profile::Root,
            SerialNumber::from(1u32),
            Validity::from_now(std::time::Duration::from_secs(60)).expect("validity"),
            Name::from_str("CN=ocx-test").expect("name"),
            spki,
            &key,
        )
        .expect("builder")
        .build::<p256::ecdsa::DerSignature>()
        .expect("build");
        let mut cert = Certificate::from_der(&built.to_der().expect("der")).expect("reparse");
        let list = cert.tbs_certificate.extensions.insert(Vec::new());
        for (oid, value) in extensions {
            list.push(Extension {
                extn_id: ObjectIdentifier::new_unwrap(oid),
                critical: false,
                extn_value: OctetString::new(value.clone()).expect("octets"),
            });
        }
        cert
    }

    fn arc(suffix: u32) -> String {
        format!("{FULCIO_ARC}.{suffix}")
    }

    #[test]
    fn c008_der_claim_matches_only_the_exact_text() {
        use ocx_trust::ClaimEncoding::Der;
        let cert = synthesized(&[(&arc(15), der_utf8("66670819"))]);
        assert!(claims_match(&cert, &[claim(15, Der, "66670819")]));
        assert!(!claims_match(&cert, &[claim(15, Der, "66670818")]));
        assert!(!claims_match(&cert, &[claim(15, Der, "6667081")]));
    }

    #[test]
    fn c008_every_listed_claim_must_match() {
        use ocx_trust::ClaimEncoding::Der;
        let cert = synthesized(&[(&arc(15), der_utf8("1")), (&arc(17), der_utf8("2"))]);
        assert!(claims_match(&cert, &[claim(15, Der, "1"), claim(17, Der, "2")]));
        assert!(!claims_match(&cert, &[claim(15, Der, "1"), claim(17, Der, "3")]));
    }

    #[test]
    fn c008_raw_claim_reads_the_value_as_the_string_itself() {
        use ocx_trust::ClaimEncoding::{Der, Raw};
        let cert = synthesized(&[(&arc(1), b"https://issuer.example".to_vec())]);
        assert!(claims_match(&cert, &[claim(1, Raw, "https://issuer.example")]));
        // The same bytes are not a DER string, and there is no fallback between the two decodes.
        assert!(!claims_match(&cert, &[claim(1, Der, "https://issuer.example")]));
        // A DER-wrapped value read as Raw carries its header bytes, so the bare text never matches.
        let wrapped = synthesized(&[(&arc(8), der_utf8("abc"))]);
        assert!(!claims_match(&wrapped, &[claim(8, Raw, "abc")]));
    }

    #[test]
    fn c008_raw_claim_with_invalid_utf8_is_a_non_match() {
        use ocx_trust::ClaimEncoding::Raw;
        let cert = synthesized(&[(&arc(1), vec![0xff, 0xfe])]);
        assert!(!claims_match(&cert, &[claim(1, Raw, "\u{fffd}\u{fffd}")]));
    }

    #[test]
    fn c008_non_minimal_der_length_is_a_non_match() {
        use ocx_trust::ClaimEncoding::Der;
        let minimal = synthesized(&[(&arc(9), vec![0x0c, 0x03, b'a', b'b', b'c'])]);
        assert!(claims_match(&minimal, &[claim(9, Der, "abc")]));
        let long_form = synthesized(&[(&arc(9), vec![0x0c, 0x81, 0x03, b'a', b'b', b'c'])]);
        assert!(!claims_match(&long_form, &[claim(9, Der, "abc")]));
        let two_octet = synthesized(&[(&arc(9), vec![0x0c, 0x82, 0x00, 0x03, b'a', b'b', b'c'])]);
        assert!(!claims_match(&two_octet, &[claim(9, Der, "abc")]));
        let indefinite = synthesized(&[(&arc(9), vec![0x0c, 0x80, b'a', b'b', b'c', 0x00, 0x00])]);
        assert!(!claims_match(&indefinite, &[claim(9, Der, "abc")]));
    }

    /// A 128-byte value needs the long form, and `0x81 0x80` is its one minimal encoding.
    #[test]
    fn c008_a_minimal_long_form_length_matches() {
        use ocx_trust::ClaimEncoding::Der;
        let text = "a".repeat(128);
        let mut value = vec![0x0c, 0x81, 0x80];
        value.extend_from_slice(text.as_bytes());
        assert!(claims_match(&synthesized(&[(&arc(9), value)]), &[claim(9, Der, &text)]));
    }

    #[test]
    fn c008_a_tag_other_than_utf8_string_is_a_non_match() {
        use ocx_trust::ClaimEncoding::Der;
        let printable = synthesized(&[(&arc(9), vec![0x13, 0x03, b'a', b'b', b'c'])]);
        assert!(!claims_match(&printable, &[claim(9, Der, "abc")]));
    }

    #[test]
    fn c008_trailing_bytes_after_the_der_string_are_a_non_match() {
        use ocx_trust::ClaimEncoding::Der;
        let mut value = der_utf8("abc");
        value.push(0x00);
        let cert = synthesized(&[(&arc(9), value)]);
        assert!(!claims_match(&cert, &[claim(9, Der, "abc")]));
    }

    #[test]
    fn c008_present_empty_matches_only_the_empty_string_and_absent_never() {
        use ocx_trust::ClaimEncoding::Der;
        let present = synthesized(&[(&arc(16), der_utf8(""))]);
        assert!(claims_match(&present, &[claim(16, Der, "")]));
        assert!(!claims_match(&present, &[claim(16, Der, "x")]));
        let absent = synthesized(&[]);
        assert!(!claims_match(&absent, &[claim(16, Der, "")]));
    }

    #[test]
    fn c008_matching_is_byte_exact() {
        use ocx_trust::ClaimEncoding::Der;
        let cert = synthesized(&[(&arc(9), der_utf8("Repo/Name"))]);
        assert!(claims_match(&cert, &[claim(9, Der, "Repo/Name")]));
        assert!(!claims_match(&cert, &[claim(9, Der, "repo/name")]));
    }

    #[test]
    fn c008_a_duplicated_claim_extension_is_a_non_match() {
        use ocx_trust::ClaimEncoding::Der;
        let cert = synthesized(&[(&arc(15), der_utf8("1")), (&arc(15), der_utf8("1"))]);
        assert!(!claims_match(&cert, &[claim(15, Der, "1")]));
    }

    #[test]
    fn c008_a_claim_with_a_foreign_oid_never_matches() {
        let cert = synthesized(&[("1.2.3.4", der_utf8("x"))]);
        let foreign = ocx_trust::CompiledClaim {
            selector: ocx_trust::ClaimSelector {
                oid: "not an oid".to_owned(),
                encoding: ocx_trust::ClaimEncoding::Der,
            },
            expected: "x".to_owned(),
        };
        assert!(!claims_match(&cert, &[foreign]));
    }

    // ── issuer and SAN are read only when present exactly once ─────────

    fn san_uri(uri: &str) -> Vec<u8> {
        use x509_cert::der::Encode;
        use x509_cert::der::asn1::Ia5String;
        SubjectAltName(vec![GeneralName::UniformResourceIdentifier(
            Ia5String::new(uri).expect("ia5"),
        )])
        .to_der()
        .expect("san der")
    }

    #[test]
    fn c008_issuer_extension_read_once_matches_and_duplicated_does_not() {
        let once = synthesized(&[(&arc(8), der_utf8("https://issuer.example"))]);
        assert_eq!(oidc_issuer(&once).as_deref(), Some("https://issuer.example"));
        let twice = synthesized(&[
            (&arc(8), der_utf8("https://issuer.example")),
            (&arc(8), der_utf8("https://issuer.example")),
        ]);
        assert_eq!(oidc_issuer(&twice), None);
    }

    #[test]
    fn c008_san_read_once_matches_and_duplicated_does_not() {
        let once = synthesized(&[("2.5.29.17", san_uri("https://signer.example/a"))]);
        assert_eq!(subject_identity(&once).as_deref(), Some("https://signer.example/a"));
        let twice = synthesized(&[
            ("2.5.29.17", san_uri("https://signer.example/a")),
            ("2.5.29.17", san_uri("https://signer.example/a")),
        ]);
        assert_eq!(subject_identity(&twice), None);
    }

    #[test]
    fn c008_a_certificate_with_only_the_v1_issuer_never_matches_the_issuer() {
        // `.1.1` carries the URL raw and is not read: only `.1.8` is, so the policy is refused.
        let cert = synthesized(&[
            (&arc(1), b"https://issuer.example".to_vec()),
            ("2.5.29.17", san_uri("https://signer.example/a")),
        ]);
        assert_eq!(oidc_issuer(&cert), None);
        let der = {
            use x509_cert::der::Encode;
            cert.to_der().expect("der")
        };
        let policies = [ocx_trust::CompiledPolicy::exact(
            "https://signer.example/a".to_owned(),
            "https://issuer.example".to_owned(),
        )];
        assert!(matches!(
            matching_policies(&der, &policies),
            Err(VerifyErrorKind::IssuerMismatch)
        ));
    }

    // ── against a real GitHub Actions Fulcio leaf ──────────────────────

    const GITHUB_LEAF_PEM: &str = include_str!("../../../../test/tests/fixtures/public_leaf/github_actions_leaf.pem");
    const GITHUB_LEAF_RECORD: &str =
        include_str!("../../../../test/tests/fixtures/public_leaf/github_actions_leaf.json");

    fn github_leaf() -> Certificate {
        let der = pem::parse(GITHUB_LEAF_PEM)
            .expect("the recorded leaf is PEM")
            .into_contents();
        parse_certificate(&der).expect("the recorded leaf parses")
    }

    fn recorded_claims() -> std::collections::BTreeMap<u32, String> {
        let record: serde_json::Value = serde_json::from_str(GITHUB_LEAF_RECORD).expect("sidecar is JSON");
        record["claims"]
            .as_object()
            .expect("claims object")
            .iter()
            .map(|(oid, value)| {
                let suffix = oid.strip_prefix(&format!("{FULCIO_ARC}.")).expect("fulcio arc");
                (
                    suffix.parse().expect("numeric suffix"),
                    value.as_str().expect("string claim").to_owned(),
                )
            })
            .collect()
    }

    fn encoding_for(suffix: u32) -> ocx_trust::ClaimEncoding {
        if suffix <= 6 {
            ocx_trust::ClaimEncoding::Raw
        } else {
            ocx_trust::ClaimEncoding::Der
        }
    }

    #[test]
    fn c008_the_recorded_github_leaf_matches_every_recorded_claim() {
        let cert = github_leaf();
        let recorded = recorded_claims();
        // Floor on the reader: the record names the claims the plan lists, so an empty sidecar cannot pass this.
        for required in [8, 9, 10, 15, 17, 20] {
            assert!(recorded.contains_key(&required), "sidecar lacks .1.{required}");
        }
        for (suffix, expected) in &recorded {
            assert!(
                claims_match(&cert, &[claim(*suffix, encoding_for(*suffix), expected)]),
                ".1.{suffix} did not match its recorded value"
            );
        }
        assert!(!recorded.contains_key(&23), ".1.23 is recorded absent");
        assert!(!claims_match(&cert, &[claim(23, ocx_trust::ClaimEncoding::Der, "")]));
    }

    #[test]
    fn c008_the_recorded_github_leaf_is_rejected_on_a_changed_build_signer_uri() {
        let cert = github_leaf();
        let recorded = recorded_claims();
        let uri = recorded[&9].clone();
        assert!(claims_match(&cert, &[claim(9, ocx_trust::ClaimEncoding::Der, &uri)]));
        let changed = uri.replace("@refs/heads/master", "@refs/heads/other");
        assert_ne!(changed, uri);
        assert!(!claims_match(
            &cert,
            &[claim(9, ocx_trust::ClaimEncoding::Der, &changed)]
        ));
    }

    #[test]
    fn c008_the_recorded_github_leaf_admits_a_keyless_policy_on_ids_and_refuses_another_owner() {
        use x509_cert::der::Encode;
        let cert = github_leaf();
        let recorded = recorded_claims();
        let der = cert.to_der().expect("der");
        let policy = |owner_id: &str| ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![ocx_trust::PolicyBackend::Keyless(ocx_trust::CompiledKeyless {
                identity: None,
                issuer: recorded[&8].clone(),
                claims: vec![
                    claim(15, ocx_trust::ClaimEncoding::Der, &recorded[&15]),
                    claim(17, ocx_trust::ClaimEncoding::Der, owner_id),
                ],
            })],
        };
        assert_eq!(
            matching_policies(&der, &[policy(&recorded[&17])])
                .expect("owner id matches")
                .len(),
            1
        );
        assert!(matching_policies(&der, &[policy("1")]).is_err());
    }
}
