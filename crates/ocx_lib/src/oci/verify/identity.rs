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
        // A policy's `backends` are an ANY-of set, so one satisfied keyless
        // signer admits the policy. A `Key` backend contributes nothing here and
        // must not: this function matches a Fulcio certificate, and a key
        // signature carries none — so a policy whose signers are all
        // `kind = "key"` never matches a keyless artifact, which is spec D5's
        // rule falling out of the type rather than being restated.
        //
        // Exhaustive on purpose: a third `PolicyBackend` variant has to break
        // this match rather than silently fall through as "not a keyless match".
        let mut policy_matched = false;
        for backend in &policy.backends {
            let keyless = match backend {
                PolicyBackend::Keyless(keyless) => keyless,
                PolicyBackend::Key(_) => continue,
            };
            let identity_ok = san.as_deref().is_some_and(|san| keyless.identity.matches(san));
            any_identity_matched |= identity_ok;
            policy_matched |= identity_ok && issuer.as_deref() == Some(keyless.issuer.as_str());
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

/// Verify a signature against an ANY-of set of compiled trust policies,
/// returning every policy whose pinned public key produced it.
///
/// The key-mode twin of [`matching_policies`], and a separate function rather
/// than a branch inside it on purpose: that one is handed a certificate, this
/// one a message and a signature, and one function taking both would have to
/// accept a call shape in which half its arguments mean nothing.
///
/// `message` is the bytes the signature covers, and never the base64 text of
/// them. Which bytes those are is the caller's wire shape, not a constant: the
/// DSSE Pre-Authentication Encoding ([`crate::oci::attest::dsse::pae`]) on the
/// bundle path, and the raw simplesigning payload exactly as the registry
/// served it on the sidecar path — cosign signs those bytes directly, with no
/// PAE wrapper around them.
///
/// # Which refusal, and why it is not the identity one
///
/// A signature no policy key verifies is either "signed by a key nobody here
/// trusts" or "trusted key, tampered signature". A verifier holding only public
/// keys cannot distinguish them — both are `verify_signature` returning an error
/// for every key it has. Both therefore land on
/// [`VerifyErrorKind::SignatureInvalid`] (exit 65), whose rendering ("signature
/// verification failed") is the one sentence true of both readings: at least one
/// trusted key was tried and none accepted these bytes.
///
/// [`VerifyErrorKind::IdentityMismatch`] (exit 77) was the older answer and was
/// wrong twice over. It renders as "certificate identity mismatch" on a path
/// that carries no certificate and reads no identity, so it names material that
/// does not exist; and it hides a bad signature from every caller scripting 65
/// as "this artifact did not verify", handing them a permissions code they
/// cannot act on.
///
/// # The one case that IS about identity
///
/// A policy set carrying no [`PolicyBackend::Key`] at all — every signer is
/// `kind = "keyless"` — never even reaches `verify_signature`. Nothing was
/// measured about the signature there, so reporting it invalid would assert a
/// corruption never observed; the refusal is
/// [`VerifyErrorKind::IdentityMismatch`], which is the honest verdict: this
/// artifact is key-signed and nobody here trusts a key. That is spec D5's
/// direction for a certificate ([`matching_policies`]) mirrored, and it is
/// decided on the policy set rather than on the verification outcome so the two
/// answers cannot blur into one.
///
/// # Errors
///
/// [`VerifyErrorKind::SignatureInvalid`] when at least one policy key was tried
/// and none verified the signature; [`VerifyErrorKind::IdentityMismatch`] when
/// the policy set names no key at all. Never any other kind: there is no
/// material here that could be malformed in a way worth a distinct code.
pub(crate) fn matching_key_policies<'a>(
    message: &[u8],
    signature: &[u8],
    policies: &'a [crate::trust::CompiledPolicy],
) -> Result<Vec<&'a crate::trust::CompiledPolicy>, VerifyErrorKind> {
    let mut matched = Vec::new();
    let mut any_key_tried = false;
    for policy in policies {
        // Exhaustive for the same reason the sibling function's match is: a
        // third `PolicyBackend` variant must break this match rather than
        // silently fall through as "not a key match".
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
            VerifyErrorKind::SignatureInvalid
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

    // ── Backend sets (WP9a) ──────────────────────────────────────────────────

    /// The real Fulcio leaf G0 captured: SAN `ocx-test@example.com`, issuer
    /// `http://dex:5556/dex`. Used rather than a synthesized certificate so the
    /// assertions below are about policy matching, not about cert construction.
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/keyless_bundle.json");
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
    fn key_backend() -> crate::trust::PolicyBackend {
        const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../../../test/tests/fixtures/golden/keys/cosign.pub");
        crate::trust::PolicyBackend::Key(
            sigstore::crypto::CosignVerificationKey::try_from_pem(GOLDEN_PUBLIC_KEY_PEM.as_bytes())
                .expect("cosign.pub is an SPKI PEM"),
        )
    }

    fn keyless_backend(identity: &str, issuer: &str) -> crate::trust::PolicyBackend {
        crate::trust::PolicyBackend::Keyless(crate::trust::CompiledKeyless {
            identity: crate::trust::IdentityRule::Exact(identity.to_string()),
            issuer: issuer.to_string(),
        })
    }

    /// **Spec D5, falling out of the type rather than being restated.** A policy
    /// whose signers are all `kind = "key"` names nobody who signs with a
    /// certificate, so a keyless artifact must not match it — a key backend
    /// contributes nothing here and must never be read as "no objection".
    #[test]
    fn a_key_only_policy_never_matches_a_certificate() {
        let policies = [crate::trust::CompiledPolicy {
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
        let policies = [crate::trust::CompiledPolicy {
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
        let policies = [crate::trust::CompiledPolicy {
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
        let wrong_issuer = [crate::trust::CompiledPolicy {
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

        let wrong_identity = [crate::trust::CompiledPolicy {
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
        let policies = [crate::trust::CompiledPolicy {
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
        let policies = [crate::trust::CompiledPolicy {
            builder: None,
            backends: vec![keyless_backend(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
        }];
        let error = matching_key_policies(b"the signed message", b"not a signature", &policies)
            .expect_err("a keyless-only policy admits no key signature");
        assert!(matches!(error, VerifyErrorKind::IdentityMismatch), "got {error:?}");
    }
}
