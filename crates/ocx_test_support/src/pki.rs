// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only PKI for the extra-CA-roots handshake tests: a minted P-256 root, a `localhost` / `127.0.0.1`
//! leaf it signed, and an in-process TLS server answering every request with an empty `200 OK`.

use std::net::SocketAddr;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use p256::ecdsa::SigningKey;
use p256::elliptic_curve::rand_core::OsRng;
use p256::pkcs8::EncodePrivateKey as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use x509_cert::builder::{Builder as _, CertificateBuilder, Profile};
use x509_cert::der::Encode as _;
use x509_cert::der::asn1::{Ia5String, OctetString};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{ExtendedKeyUsage, SubjectAltName};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::Validity;

/// A root, a leaf it signed, and the leaf's private key.
pub struct TestPki {
    pub root_der: Vec<u8>,
    pub leaf_der: Vec<u8>,
    pub leaf_key_pkcs8: Vec<u8>,
}

fn spki(key: &SigningKey) -> SubjectPublicKeyInfoOwned {
    SubjectPublicKeyInfoOwned::from_key(*key.verifying_key()).expect("spki")
}

fn validity() -> Validity {
    Validity::from_now(Duration::from_secs(3600)).expect("validity")
}

/// A self-signed CA root; `Profile::Root` adds the CA extensions a trust anchor needs.
pub fn mint_root(name: &str) -> (SigningKey, Vec<u8>) {
    let key = SigningKey::random(&mut OsRng);
    let builder = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(1_u32),
        validity(),
        Name::from_str(name).expect("name"),
        spki(&key),
        &key,
    )
    .expect("builder");
    let der = builder
        .build::<p256::ecdsa::DerSignature>()
        .expect("build")
        .to_der()
        .expect("der");
    (key, der)
}

/// A version-2 self-signed root, which webpki rejects with `BadEncoding` and CryptoAPI and
/// Security.framework accept, so only the Linux platform probe refuses it.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn mint_v2_root() -> Vec<u8> {
    use p256::ecdsa::signature::Signer as _;
    use x509_cert::der::asn1::BitString;
    use x509_cert::spki::DynSignatureAlgorithmIdentifier as _;

    let key = SigningKey::random(&mut OsRng);
    let name = Name::from_str("CN=ocx-test-v2-ca").expect("name");
    let tbs = x509_cert::TbsCertificate {
        version: x509_cert::Version::V2,
        serial_number: SerialNumber::from(3_u32),
        signature: key.signature_algorithm_identifier().expect("algorithm"),
        issuer: name.clone(),
        validity: validity(),
        subject: name,
        subject_public_key_info: spki(&key),
        issuer_unique_id: Some(BitString::from_bytes(&[0xAB]).expect("bit string")),
        subject_unique_id: None,
        extensions: None,
    };
    let tbs_der = tbs.to_der().expect("tbs der");
    let signature: p256::ecdsa::DerSignature = key.sign(&tbs_der);
    let certificate = x509_cert::Certificate {
        signature_algorithm: tbs.signature.clone(),
        tbs_certificate: tbs,
        signature: BitString::from_bytes(signature.as_bytes()).expect("bit string"),
    };
    certificate.to_der().expect("der")
}

impl TestPki {
    pub fn mint() -> Self {
        let (root_key, root_der) = mint_root("CN=ocx-test-ca");
        let leaf_key = SigningKey::random(&mut OsRng);
        let mut builder = CertificateBuilder::new(
            Profile::Leaf {
                issuer: Name::from_str("CN=ocx-test-ca").expect("name"),
                enable_key_agreement: false,
                enable_key_encipherment: false,
            },
            SerialNumber::from(2_u32),
            validity(),
            Name::from_str("CN=localhost").expect("name"),
            spki(&leaf_key),
            &root_key,
        )
        .expect("builder");
        builder
            .add_extension(&SubjectAltName(vec![
                GeneralName::DnsName(Ia5String::new("localhost").expect("ia5")),
                GeneralName::IpAddress(OctetString::new([127, 0, 0, 1]).expect("octets")),
            ]))
            .expect("san");
        // Without `serverAuth` macOS refuses the leaf (`EkuError`); webpki and CryptoAPI do not notice.
        builder
            .add_extension(&ExtendedKeyUsage(vec![
                x509_cert::der::oid::db::rfc5280::ID_KP_SERVER_AUTH,
            ]))
            .expect("eku");
        let leaf_der = builder
            .build::<p256::ecdsa::DerSignature>()
            .expect("build")
            .to_der()
            .expect("der");
        let leaf_key_pkcs8 = leaf_key.to_pkcs8_der().expect("pkcs8").as_bytes().to_vec();
        Self {
            root_der,
            leaf_der,
            leaf_key_pkcs8,
        }
    }

    /// The root as a one-block LF PEM bundle, the form `OCX_EXTRA_CA_CERTS` takes.
    pub fn root_pem(&self) -> String {
        pem_block("CERTIFICATE", &self.root_der)
    }
}

/// One PEM block, LF line endings (the `pem` crate defaults to CRLF).
pub fn pem_block(tag: &str, der: &[u8]) -> String {
    pem::encode_config(
        &pem::Pem::new(tag, der),
        pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF),
    )
}

/// An HTTPS server on an ephemeral loopback port presenting `pki`'s leaf, answering every request with `200 OK`.
pub async fn serve_https(pki: &TestPki) -> SocketAddr {
    use tokio_rustls::TlsAcceptor;
    use tokio_rustls::rustls::ServerConfig;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![
                pki_types::CertificateDer::from(pki.leaf_der.clone()),
                pki_types::CertificateDer::from(pki.root_der.clone()),
            ],
            pki_types::PrivateKeyDer::Pkcs8(pki_types::PrivatePkcs8KeyDer::from(pki.leaf_key_pkcs8.clone())),
        )
        .expect("the minted leaf and key form a server identity");
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local address");
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match tls.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&chunk[..read]),
                    }
                }
                let _ = tls
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
                let _ = tls.shutdown().await;
            });
        }
    });
    addr
}

/// Asserts the platform verifier refused the minted root: `UnknownIssuer` (webpki, CryptoAPI) or macOS's
/// "certificate is not trusted"; a timeout, refused connection or name mismatch matches neither.
pub fn assert_untrusted_root(chain: &str) {
    assert!(
        chain.contains("UnknownIssuer") || chain.contains("certificate is not trusted"),
        "expected an untrusted-root refusal in: {chain}"
    );
}

/// Every `Display` down the `source` chain, joined, so an assertion sees rustls's `UnknownIssuer`.
pub fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
