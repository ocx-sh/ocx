// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Detached in-toto statements: a signature over a file's bytes that lives in a file beside it,
//! not in a registry referrer. The bundle is the same Sigstore v0.3 DSSE bundle a package attestation carries.

use ocx_oci::Algorithm;
use ocx_trust::CompiledPolicy;
use ocx_trust::key_ref::KeyBackendKind;
use url::Url;

use crate::attest::predicate::PredicateType;
use crate::attest::statement;
use crate::sign::pipeline::guard_dialed_endpoints;
use crate::sign::{OidcToken, SignErrorKind, Signer, SignerIdentity};
use crate::verify::pipeline::verify_detached_statement;
use crate::verify::{TrustRoot, VerifyContentMode, VerifyErrorKind};

/// The predicate type of a signed index catalog, the domain separator that keeps a signature the same
/// identity made over anything else from passing as a catalog signature.
pub const INDEX_CATALOG_PREDICATE_TYPE: &str = "https://ocx.sh/attestation/index-catalog/v1";

/// Where a signing run reaches the Sigstore services, and the identity token the certificate authority takes.
pub struct SigningEndpoints<'a> {
    pub fulcio_url: &'a Url,
    pub rekor_url: &'a Url,
    /// `Some` exactly when the signer needs an identity token.
    pub token: Option<&'a OidcToken>,
    /// Hosts the registry configuration admits on a private address, for the SSRF floor on the dialed endpoints.
    pub trusted_hosts: &'a [String],
}

/// A signed statement over one payload.
#[derive(Debug, Clone)]
pub struct SignedStatement {
    /// The serialized Sigstore bundle v0.3, ready to write to a file.
    pub bundle: Vec<u8>,
    /// Who the signature is attributed to.
    pub signer: SignerIdentity,
    /// The Rekor log index; absent when no upload happened.
    pub tlog_index: Option<u64>,
}

/// A statement that passed every check.
#[derive(Debug, Clone)]
pub struct VerifiedStatement {
    /// Position, within the policy's backends, of the signer entry that matched.
    pub signer_index: usize,
    /// Who signed, as read from the verified certificate or key.
    pub signer: SignerIdentity,
    /// The Rekor log index of the verified entry, when the bundle carries one.
    pub tlog_index: Option<u64>,
}

/// Signs an in-toto Statement v1 whose single subject is `subject_name` at `sha256(payload)`.
///
/// # Errors
///
/// Any [`SignErrorKind`] the signer or the transparency log raises, and
/// [`SignErrorKind::InvalidEndpointUrl`] for a dialed Sigstore endpoint the SSRF floor refuses.
pub async fn sign_statement(
    payload: &[u8],
    subject_name: &str,
    predicate_type: &str,
    predicate: &serde_json::Value,
    signer: &dyn Signer,
    endpoints: &SigningEndpoints<'_>,
) -> Result<SignedStatement, SignErrorKind> {
    guard_dialed_endpoints(
        endpoints.trusted_hosts,
        signer,
        endpoints.fulcio_url,
        endpoints.rekor_url,
    )
    .await?;
    let internal = |error: serde_json::Error| SignErrorKind::Internal(Box::new(error));
    let predicate = serde_json::value::to_raw_value(predicate).map_err(internal)?;
    let statement = statement::build(
        subject_name,
        &Algorithm::Sha256.hash(payload),
        &PredicateType::Uri(predicate_type.to_owned()),
        &predicate,
        chrono::Utc::now(),
    )?;
    let statement_bytes = serde_json::to_vec(&statement).map_err(internal)?;

    let signed = signer
        .sign_dsse(
            &statement_bytes,
            endpoints.token,
            endpoints.fulcio_url,
            endpoints.rekor_url,
        )
        .await?;
    let identity = match signed.key_backend {
        KeyBackendKind::Keyless => SignerIdentity::Keyless {
            certificate_identity: signed.certificate_identity,
            certificate_oidc_issuer: signed.certificate_oidc_issuer,
        },
        key_backend => SignerIdentity::Key {
            public_key_hint: signed.public_key_hint.unwrap_or_default(),
            key_backend,
        },
    };
    Ok(SignedStatement {
        bundle: signed.bytes,
        signer: identity,
        tlog_index: signed.transparency_log_index,
    })
}

/// Verifies `bundle` as a signature over `payload`.
///
/// Refuses a bundle over 256 KiB or one that does not parse, a non-DSSE content, another payload
/// type, a statement with other than exactly one subject, a subject name other than `subject_name`,
/// a subject digest other than `sha256(payload)`, a predicate type other than `predicate_type` or a
/// predicate `predicate_check` rejects, and a failed signature, certificate chain or transparency proof.
/// No network is reached: a Rekor key the trust root does not pin is a refusal.
///
/// # Errors
///
/// The [`VerifyErrorKind`] of the first refusal.
pub async fn verify_statement(
    payload: &[u8],
    bundle: &[u8],
    subject_name: &str,
    predicate_type: &str,
    predicate_check: &(dyn Fn(&serde_json::Value) -> bool + Sync),
    signers: &CompiledPolicy,
    trust_root: &TrustRoot,
) -> Result<VerifiedStatement, VerifyErrorKind> {
    let verified = verify_detached_statement(
        payload,
        bundle,
        subject_name,
        predicate_type,
        predicate_check,
        signers,
        trust_root,
        &VerifyContentMode::Attestation { predicate_type: None },
    )
    .await?;
    Ok(VerifiedStatement {
        signer_index: verified.signer_index,
        signer: verified.signer,
        tlog_index: verified.tlog_index,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::sign::key_backend::{KeyBackend, StubKeyBackend};
    use crate::sign::key_signer::KeySigner;
    use ocx_trust::PolicyBackend;

    const PREDICATE_TYPE: &str = INDEX_CATALOG_PREDICATE_TYPE;
    const SUBJECT: &str = "c/index.json";
    const CATALOG: &[u8] = br#"{"schema_version":1,"expires":"2030-01-01T00:00:00Z","packages":{}}"#;
    const GOLDEN_KEY_BUNDLE: &str = include_str!("../../../test/tests/fixtures/golden/key_bundle.json");
    const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../test/tests/fixtures/golden/keys/cosign.pub");

    fn urls() -> (Url, Url) {
        (
            Url::parse("https://fulcio.example").unwrap(),
            Url::parse("https://rekor.example").unwrap(),
        )
    }

    fn stub_signer() -> KeySigner {
        KeySigner::new(Arc::new(StubKeyBackend::fixed()), None)
    }

    fn key_policy_from_pem(pem: &str) -> CompiledPolicy {
        CompiledPolicy {
            builder: None,
            backends: vec![PolicyBackend::Key(
                sigstore::crypto::CosignVerificationKey::try_from_pem(pem.as_bytes()).expect("an SPKI PEM"),
            )],
        }
    }

    fn stub_policy() -> CompiledPolicy {
        let der = StubKeyBackend::fixed().public_key_der().to_vec();
        key_policy_from_pem(&pem::encode(&pem::Pem::new("PUBLIC KEY", der)))
    }

    fn registry_is(registry: &'static str) -> impl Fn(&serde_json::Value) -> bool {
        move |predicate| predicate.get("registry").and_then(serde_json::Value::as_str) == Some(registry)
    }

    async fn sign_with(signer: &dyn Signer, payload: &[u8], subject: &str, predicate_type: &str) -> SignedStatement {
        let (fulcio, rekor) = urls();
        let endpoints = SigningEndpoints {
            fulcio_url: &fulcio,
            rekor_url: &rekor,
            token: None,
            trusted_hosts: &[],
        };
        sign_statement(
            payload,
            subject,
            predicate_type,
            &serde_json::json!({ "registry": "ocx.sh" }),
            signer,
            &endpoints,
        )
        .await
        .expect("a key signer signs")
    }

    async fn verify(
        payload: &[u8],
        bundle: &[u8],
        subject: &str,
        predicate_type: &str,
        registry: &'static str,
        policy: &CompiledPolicy,
    ) -> Result<VerifiedStatement, VerifyErrorKind> {
        verify_statement(
            payload,
            bundle,
            subject,
            predicate_type,
            &registry_is(registry),
            policy,
            &TrustRoot::default(),
        )
        .await
    }

    /// Index signing reaches the same SSRF floor package signing does: a private-address endpoint a keyless
    /// signer would dial is refused before any identity token or request is used.
    #[tokio::test]
    async fn a_private_address_sigstore_endpoint_is_refused() {
        let private = Url::parse("http://10.1.2.3").expect("a url");
        let endpoints = SigningEndpoints {
            fulcio_url: &private,
            rekor_url: &private,
            token: None,
            trusted_hosts: &[],
        };
        let error = sign_statement(
            CATALOG,
            SUBJECT,
            PREDICATE_TYPE,
            &serde_json::json!({}),
            &crate::sign::KeylessSigner::new(),
            &endpoints,
        )
        .await
        .expect_err("a private endpoint is refused");
        assert!(matches!(error, SignErrorKind::InvalidEndpointUrl { .. }), "{error:?}");
    }

    /// The round trip the refusals below are only meaningful against.
    #[tokio::test]
    async fn a_key_signed_statement_verifies_and_names_the_matching_entry() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, PREDICATE_TYPE).await;
        assert!(matches!(signed.signer, SignerIdentity::Key { .. }));
        assert_eq!(signed.tlog_index, None);
        let verified = verify(
            CATALOG,
            &signed.bundle,
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &stub_policy(),
        )
        .await
        .expect("the statement verifies");
        assert_eq!(verified.signer_index, 0);
        assert_eq!(verified.signer, signed.signer);
        assert_eq!(verified.tlog_index, None);
    }

    /// The matched entry's position is reported, not always the first.
    #[tokio::test]
    async fn the_second_of_two_entries_is_the_one_reported() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, PREDICATE_TYPE).await;
        let other = key_policy_from_pem(GOLDEN_PUBLIC_KEY_PEM);
        let mut both = other;
        both.backends.extend(stub_policy().backends);
        let verified = verify(CATALOG, &signed.bundle, SUBJECT, PREDICATE_TYPE, "ocx.sh", &both)
            .await
            .expect("the second entry matches");
        assert_eq!(verified.signer_index, 1);
    }

    #[tokio::test]
    async fn a_changed_payload_is_refused() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, PREDICATE_TYPE).await;
        let result = verify(
            b"{\"schema_version\":1}",
            &signed.bundle,
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &stub_policy(),
        )
        .await;
        assert!(
            matches!(result, Err(VerifyErrorKind::StatementSubjectMismatch { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_right_digest_under_a_wrong_subject_name_is_refused() {
        let signed = sign_with(&stub_signer(), CATALOG, "c/other.json", PREDICATE_TYPE).await;
        let result = verify(
            CATALOG,
            &signed.bundle,
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &stub_policy(),
        )
        .await;
        assert!(
            matches!(result, Err(VerifyErrorKind::StatementSubjectMismatch { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_right_subject_under_another_predicate_type_is_refused() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, "https://example.com/other/v1").await;
        let result = verify(
            CATALOG,
            &signed.bundle,
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &stub_policy(),
        )
        .await;
        assert!(
            matches!(result, Err(VerifyErrorKind::PredicateTypeMismatch { .. })),
            "{result:?}"
        );
    }

    /// The image-signature predicate type is its own content kind and never a detached statement.
    #[tokio::test]
    async fn a_right_subject_under_the_image_signature_predicate_type_is_refused() {
        let signed = sign_with(
            &stub_signer(),
            CATALOG,
            SUBJECT,
            crate::attest::COSIGN_SIGN_PREDICATE_TYPE,
        )
        .await;
        let result = verify(
            CATALOG,
            &signed.bundle,
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &stub_policy(),
        )
        .await;
        assert!(matches!(result, Err(VerifyErrorKind::NoUsableBundle)), "{result:?}");
    }

    /// One identity signing two indexes cannot have one catalog accepted for the other.
    #[tokio::test]
    async fn a_predicate_the_caller_refuses_is_refused() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, PREDICATE_TYPE).await;
        let result = verify(
            CATALOG,
            &signed.bundle,
            SUBJECT,
            PREDICATE_TYPE,
            "other.example",
            &stub_policy(),
        )
        .await;
        assert!(
            matches!(result, Err(VerifyErrorKind::PredicateTypeMismatch { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_statement_with_two_subjects_is_refused_even_when_one_binds() {
        use sha2::Digest as _;
        let digest = hex::encode(sha2::Sha256::digest(CATALOG));
        let statement = format!(
            r#"{{"_type":"https://in-toto.io/Statement/v1","subject":[{{"name":"{SUBJECT}","digest":{{"sha256":"{digest}"}}}},{{"name":"c/other.json","digest":{{"sha256":"{}"}}}}],"predicateType":"{PREDICATE_TYPE}","predicate":{{"registry":"ocx.sh"}}}}"#,
            "11".repeat(32)
        );
        let (fulcio, rekor) = urls();
        let bundle = stub_signer()
            .sign_dsse(statement.as_bytes(), None, &fulcio, &rekor)
            .await
            .expect("the signer signs any statement");
        let result = verify(
            CATALOG,
            &bundle.bytes,
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &stub_policy(),
        )
        .await;
        assert!(
            matches!(result, Err(VerifyErrorKind::StatementSubjectMismatch { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_signature_by_another_key_is_refused() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, PREDICATE_TYPE).await;
        let result = verify(
            CATALOG,
            &signed.bundle,
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &key_policy_from_pem(GOLDEN_PUBLIC_KEY_PEM),
        )
        .await;
        assert!(matches!(result, Err(VerifyErrorKind::SignatureInvalid)), "{result:?}");
    }

    /// 256 KiB is the ceiling; the file is refused before it is parsed.
    #[tokio::test]
    async fn a_bundle_over_256_kib_is_refused_unparsed() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, PREDICATE_TYPE).await;
        let mut padded = signed.bundle.clone();
        padded.extend(std::iter::repeat_n(b' ', 300 * 1024));
        assert!(
            serde_json::from_slice::<serde_json::Value>(&padded).is_ok(),
            "the padded file still parses, so only the cap can refuse it"
        );
        let result = verify(CATALOG, &padded, SUBJECT, PREDICATE_TYPE, "ocx.sh", &stub_policy()).await;
        assert!(matches!(result, Err(VerifyErrorKind::BundleParseFailed)), "{result:?}");
    }

    #[tokio::test]
    async fn a_bundle_that_is_not_json_is_refused() {
        let result = verify(
            CATALOG,
            b"not a bundle",
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &stub_policy(),
        )
        .await;
        assert!(matches!(result, Err(VerifyErrorKind::BundleParseFailed)), "{result:?}");
    }

    #[tokio::test]
    async fn a_message_signature_bundle_is_refused() {
        let bundle = br#"{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json","verificationMaterial":{"publicKey":{"hint":"x"}},"messageSignature":{"messageDigest":{"algorithm":"SHA2_256","digest":"AAAA"},"signature":"AAAA"}}"#;
        let result = verify(CATALOG, bundle, SUBJECT, PREDICATE_TYPE, "ocx.sh", &stub_policy()).await;
        assert!(matches!(result, Err(VerifyErrorKind::NoUsableBundle)), "{result:?}");
    }

    /// cosign's own image signature under the identity the policy trusts is not a catalog signature.
    #[tokio::test]
    async fn an_image_signature_replayed_as_the_catalog_signature_is_refused() {
        let result = verify(
            CATALOG,
            GOLDEN_KEY_BUNDLE.as_bytes(),
            SUBJECT,
            PREDICATE_TYPE,
            "ocx.sh",
            &key_policy_from_pem(GOLDEN_PUBLIC_KEY_PEM),
        )
        .await;
        assert!(matches!(result, Err(VerifyErrorKind::NoUsableBundle)), "{result:?}");
    }

    #[tokio::test]
    async fn signing_returns_the_statement_it_built() {
        let signed = sign_with(&stub_signer(), CATALOG, SUBJECT, PREDICATE_TYPE).await;
        let bundle: serde_json::Value = serde_json::from_slice(&signed.bundle).expect("a bundle is JSON");
        let payload = base64_decode(bundle["dsseEnvelope"]["payload"].as_str().expect("a payload"));
        let statement: serde_json::Value = serde_json::from_slice(&payload).expect("a statement");
        assert_eq!(statement["subject"].as_array().map(Vec::len), Some(1));
        assert_eq!(statement["subject"][0]["name"], SUBJECT);
        assert_eq!(statement["predicateType"], PREDICATE_TYPE);
        assert_eq!(statement["predicate"], serde_json::json!({ "registry": "ocx.sh" }));
    }

    fn base64_decode(text: &str) -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(text).expect("base64")
    }
}
