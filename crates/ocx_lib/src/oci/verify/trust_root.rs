// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sigstore trust material.
//!
//! [`TrustRoot`] holds the three kinds of key material a keyless verification
//! needs: the Fulcio CA certificate(s) that anchor the signing certificate's
//! chain, the certificate-transparency (CTFE) log keys that check the SCT
//! embedded in that certificate, and the Rekor log key that checks the Signed
//! Entry Timestamp.
//!
//! **Nothing here parses ASN.1, X.509 or the `TrustedRoot` wire format by
//! hand.** Every parse is `sigstore`'s own: `SigstoreTrustRoot` for the
//! protobuf-JSON document, `x509_cert` for a DER certificate body. The type
//! implements [`sigstore::trust::TrustRoot`], so it plugs straight into
//! `sigstore::bundle::verify::Verifier`, which owns chain building, validity
//! windows and SCT verification.
//!
//! Construction paths:
//!
//! - [`TrustRoot::load_trusted_root_json`] — a Sigstore `TrustedRoot` JSON
//!   document: Fulcio CAs, CTFE keys and the pinned Rekor key. The
//!   `--sigstore-trusted-root` / `OCX_SIGSTORE_TRUSTED_ROOT` / `[trust.sigstore]`
//!   air-gapped seam. No network.
//! - [`TrustRoot::from_material`] — rebuild from the trust-root cache.
//! - [`TrustRoot::load_embedded`] — the public-good Sigstore root, fetched and
//!   verified over TUF by `sigstore`'s `tough`-backed client.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use pki_types::CertificateDer;
use sigstore::trust::TrustRoot as SigstoreTrustRootTrait;
use sigstore::trust::sigstore::SigstoreTrustRoot;

use super::error::{TrustRootLoadReason, VerifyErrorKind};

/// Reject a certificate body that is not a parseable X.509 certificate.
///
/// The parse is `x509_cert`'s, not ours: a structural byte check cannot tell a
/// DER `SEQUENCE` from a certificate, and a trust anchor that fails to parse
/// here would fail deep inside chain building with a far worse message.
fn parse_certificate(der: &[u8]) -> Result<(), String> {
    use x509_cert::der::Decode;
    x509_cert::Certificate::from_der(der)
        .map(|_| ())
        .map_err(|e| format!("invalid X.509 certificate in trust root: {e}"))
}

/// Whole-operation deadline for the public-good TUF trust-root fetch.
///
/// Nothing below this call bounds the wait: `sigstore` builds the TUF client's
/// `reqwest::Client` itself with no connect or request timeout and no injection
/// point, and wraps it in its own transport, which replaces `tough`'s -- so
/// `tough`'s own 30s/10s defaults never apply either. An endpoint that completes
/// the TCP handshake and then sends nothing otherwise hangs `ocx package verify`
/// forever, and through the auto-verify hook hangs every covered install.
///
/// The budget is a whole-operation one because the fetch is a chain of several
/// small sequential requests (TUF metadata, then the `trusted_root.json`
/// target), not one: a per-request number cannot be expressed here at all, since
/// the requests are `sigstore`'s. It is set to twice the per-request budget the
/// sibling trust services use for a single Fulcio or Rekor call (30s, in
/// `oci::endpoint`) -- generous enough that a slow link walking the chain is not
/// cut off mid-fetch, short enough that a blackholed endpoint fails in a minute
/// rather than never.
const TUF_FETCH_DEADLINE: Duration = Duration::from_secs(60);

/// Bound `fetch` by `deadline`, reporting expiry as
/// [`TrustRootLoadReason::TufFetchTimeout`].
///
/// Separate from [`TrustRoot::load_embedded`] because that call reaches the live
/// public-good TUF repository and cannot be pointed anywhere else -- `sigstore`
/// hardcodes the metadata base URL and builds the HTTP client itself -- so this
/// is the only seam at which the deadline is observable without a network. The
/// deadline is a parameter for the same reason: it is what lets a test drive an
/// already-elapsed budget against a never-ready future, which is decided purely
/// by poll order and so cannot flake under load.
async fn with_deadline<T>(
    deadline: Duration,
    fetch: impl Future<Output = Result<T, VerifyErrorKind>>,
) -> Result<T, VerifyErrorKind> {
    match tokio::time::timeout(deadline, fetch).await {
        Ok(fetched) => fetched,
        Err(_elapsed) => Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::TufFetchTimeout)),
    }
}

/// Sigstore trust material: Fulcio CAs, CTFE log keys, Rekor log keys.
///
/// Keys are held as DER `SubjectPublicKeyInfo` bytes, mapped by the log's
/// hex-encoded `logId` — the same shape `sigstore::trust::ManualTrustRoot`
/// uses, because the [`SigstoreTrustRootTrait`] impl below hands them straight
/// to `sigstore`'s `Verifier`.
#[derive(Debug, Clone, Default)]
pub struct TrustRoot {
    /// DER-encoded Fulcio CA certificates — the chain's trust anchors.
    ///
    /// An empty `Vec` is valid at construction and means "no trust anchors":
    /// every chain then fails to build, which is the correct fail-closed shape.
    der_certs: Vec<Vec<u8>>,

    /// CTFE (certificate-transparency) log keys, `logId` hex → DER SPKI.
    ///
    /// Empty when the material came from a bare Fulcio PEM. `sigstore`'s
    /// verifier requires the SCT in the signing certificate to verify against
    /// one of these, so an empty map means SCT verification cannot succeed.
    ctfe_keys: BTreeMap<String, Vec<u8>>,

    /// Rekor log keys, `logId` hex → DER SPKI.
    ///
    /// Present for a `TrustedRoot` JSON, a TUF fetch, or a cache load; absent
    /// for a bare Fulcio PEM, in which case the pipeline TOFU-fetches the key
    /// from the Rekor endpoint (online only).
    rekor_keys: BTreeMap<String, Vec<u8>>,
}

impl TrustRoot {
    /// Load the public-good Sigstore trust root over TUF.
    ///
    /// Delegates entirely to `sigstore`'s `tough`-backed client: it ships the
    /// TUF root of trust, enforces metadata expiry, and verifies the target
    /// hash of `trusted_root.json` before returning it. `cache_dir` is where
    /// the fetched targets are checked out so the next run can reuse them.
    ///
    /// # Errors
    /// [`VerifyErrorKind::TrustRootLoad`] with [`TrustRootLoadReason::AssetReadFailed`]
    /// when the TUF client cannot produce a trusted root, or with
    /// [`TrustRootLoadReason::TufFetchTimeout`] when it does not answer within
    /// the whole-operation TUF deadline.
    pub async fn load_embedded(cache_dir: &Path) -> Result<Self, VerifyErrorKind> {
        // Best-effort: a missing cache dir is not a reason to fail the fetch,
        // sigstore falls back to its embedded resources and the network.
        let _ = tokio::fs::create_dir_all(cache_dir).await;
        // The local mkdir above stays outside the deadline: it is already
        // best-effort, and the budget is for the network chain.
        let root = with_deadline(TUF_FETCH_DEADLINE, async {
            SigstoreTrustRoot::new(Some(cache_dir)).await.map_err(|e| {
                VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::AssetReadFailed {
                    source: Box::new(std::io::Error::other(format!("TUF trust-root fetch failed: {e}"))),
                })
            })
        })
        .await?;
        Self::harvest(&root)
    }

    /// Copy the material out of anything implementing sigstore's `TrustRoot`.
    fn harvest<R: SigstoreTrustRootTrait>(root: &R) -> Result<Self, VerifyErrorKind> {
        // As with the log keys below, `sigstore` reports "the document named
        // none" as an `Err`, so collapse it into the empty case and let the
        // `is_empty` check name the condition.
        let der_certs: Vec<Vec<u8>> = root
            .fulcio_certs()
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.as_ref().to_vec())
            .collect();
        if der_certs.is_empty() {
            return Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCertificateBlocks));
        }
        let owned = |map: BTreeMap<String, &[u8]>| -> BTreeMap<String, Vec<u8>> {
            map.into_iter().map(|(k, v)| (k, v.to_vec())).collect()
        };
        // Log keys are optional material, and `sigstore` reports an EMPTY map as
        // an error rather than an empty map -- so an `Err` here is "the document
        // named no logs", not "a key failed to decode" (decoding already
        // happened in the codec). Treating it as fatal would reject the
        // Fulcio-plus-Rekor-only trusted root a self-hosted operator ships, and
        // the bare-CA PEM path pins neither. Whether the harvested material is
        // sufficient is the pipeline's call: no CTFE key means SCT verification
        // fails, no Rekor key means the SET key is fetched (online) or the
        // offline resolve refuses.
        Ok(Self {
            der_certs,
            ctfe_keys: root.ctfe_keys().map(owned).unwrap_or_default(),
            rekor_keys: root.rekor_keys().map(owned).unwrap_or_default(),
        })
    }

    /// Rebuild trust material from the trust-root cache.
    ///
    /// No validation beyond what the cache round-tripped: the material was
    /// already accepted by a successful online verify.
    pub fn from_material(
        der_certs: Vec<Vec<u8>>,
        ctfe_keys: BTreeMap<String, Vec<u8>>,
        rekor_keys: BTreeMap<String, Vec<u8>>,
    ) -> Self {
        Self {
            der_certs,
            ctfe_keys,
            rekor_keys,
        }
    }

    /// Parse a Sigstore [`TrustedRoot`][trusted-root] JSON document.
    ///
    /// The parse is `sigstore`'s own protobuf-JSON codec, so the accepted shape
    /// is exactly what `cosign trusted-root create` emits and what the
    /// public-good TUF repository serves. Certificate-authority and log-key
    /// validity windows are applied by that codec, not re-derived here.
    ///
    /// This is the `--sigstore-trusted-root` / `OCX_SIGSTORE_TRUSTED_ROOT` /
    /// `[trust.sigstore]` air-gapped seam: it
    /// performs no TUF fetch and no metadata-expiry check, because the operator
    /// supplied the document out of band. [`TrustRoot::load_embedded`] is the
    /// path that does verify TUF metadata.
    ///
    /// # Errors
    /// [`VerifyErrorKind::TrustRootLoad`] when the document does not parse or
    /// carries no currently-valid certificate authority.
    ///
    /// [trusted-root]: https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_trustroot.proto
    pub fn load_trusted_root_json(json_bytes: &[u8]) -> Result<Self, VerifyErrorKind> {
        let root = SigstoreTrustRoot::from_trusted_root_json_unchecked(json_bytes).map_err(|e| {
            VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed {
                detail: format!("invalid TrustedRoot JSON: {e}"),
            })
        })?;
        let harvested = Self::harvest(&root)?;
        // sigstore's codec accepts a CA whose `certChain` is absent, which
        // harvests to zero anchors; `harvest` already rejects that. Re-check the
        // certificate bodies, because a `rawBytes` field is opaque to the codec.
        for der in &harvested.der_certs {
            parse_certificate(der)
                .map_err(|detail| VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail }))?;
        }
        Ok(harvested)
    }

    /// Pin a Rekor public key fetched at runtime (the TOFU path) onto this
    /// trust root.
    ///
    /// Used when the operator supplied a bare Fulcio CA PEM: the key is fetched
    /// once from the Rekor endpoint and pinned here so the rest of the batch
    /// reuses it. The log id is unknown on that path, so the key is filed under
    /// the empty id and reached through the first-key fallback.
    ///
    /// # Errors
    /// [`VerifyErrorKind::TrustRootLoad`] when `pem` is not a PEM public key.
    pub fn with_rekor_key_pem(mut self, pem: &str) -> Result<Self, VerifyErrorKind> {
        let block = pem::parse(pem).map_err(|e| {
            VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed {
                detail: format!("malformed Rekor public key PEM: {e}"),
            })
        })?;
        self.rekor_keys.insert(String::new(), block.contents().to_vec());
        Ok(self)
    }

    /// The DER-encoded Fulcio CA certificates held by this trust root.
    pub fn der_certs(&self) -> &[Vec<u8>] {
        &self.der_certs
    }

    /// The CTFE log keys, `logId` hex → DER SPKI.
    pub fn ctfe_key_map(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.ctfe_keys
    }

    /// The Rekor log keys, `logId` hex → DER SPKI.
    pub fn rekor_key_map(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.rekor_keys
    }

    /// The pinned Rekor public key as PEM, if this trust root carries one.
    ///
    /// `Some` means the verify pipeline pins this key for SET verification with
    /// no network; `None` means it TOFU-fetches the key from the Rekor endpoint.
    ///
    /// Log-id-**un**aware: `rekor_keys` is keyed by `logId` hex, so this returns
    /// whichever id sorts first, not a key chosen for any particular bundle. Use
    /// it only where the caller has no log id to select on; a caller that does
    /// have one belongs on [`TrustRoot::rekor_public_key_pem_for`], or a bundle
    /// from the second log of a rotated trust root verifies against the first
    /// log's key and reports a valid signature as corrupt data.
    pub fn rekor_public_key_pem(&self) -> Option<String> {
        self.rekor_keys
            .values()
            .next()
            .map(|der| pem::encode(&pem::Pem::new("PUBLIC KEY", der.clone())))
    }

    /// The pinned Rekor key for a specific log, as PEM — the log-id-aware
    /// selector, and the one a SET check with a bundle in hand wants.
    ///
    /// Falls back to [`TrustRoot::rekor_public_key_pem`] when the bundle's log
    /// id is not one this trust root knows — a single-log deployment writes no
    /// meaningful id, and refusing there would break every private stack.
    pub fn rekor_public_key_pem_for(&self, log_id_hex: &str) -> Option<String> {
        self.rekor_keys
            .get(log_id_hex)
            .map(|der| pem::encode(&pem::Pem::new("PUBLIC KEY", der.clone())))
            .or_else(|| self.rekor_public_key_pem())
    }
}

impl SigstoreTrustRootTrait for TrustRoot {
    fn fulcio_certs(&self) -> sigstore::errors::Result<Vec<CertificateDer<'_>>> {
        Ok(self
            .der_certs
            .iter()
            .map(|der| CertificateDer::from(der.as_slice()))
            .collect())
    }

    fn rekor_keys(&self) -> sigstore::errors::Result<BTreeMap<String, &[u8]>> {
        Ok(self.rekor_keys.iter().map(|(k, v)| (k.clone(), v.as_slice())).collect())
    }

    fn ctfe_keys(&self) -> sigstore::errors::Result<BTreeMap<String, &[u8]>> {
        Ok(self.ctfe_keys.iter().map(|(k, v)| (k.clone(), v.as_slice())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real, self-signed P-256 certificate. Synthetic "structurally valid
    /// DER" fixtures cannot be used any more, and that is the point: the loader
    /// now runs a real X.509 parse, so only a real certificate passes it.
    fn real_cert_der() -> Vec<u8> {
        use p256::ecdsa::SigningKey;
        use p256::elliptic_curve::rand_core::OsRng;
        use std::str::FromStr;
        use std::time::Duration;
        use x509_cert::builder::{Builder, CertificateBuilder, Profile};
        use x509_cert::der::Encode;
        use x509_cert::name::Name;
        use x509_cert::serial_number::SerialNumber;
        use x509_cert::spki::SubjectPublicKeyInfoOwned;
        use x509_cert::time::Validity;

        let signing_key = SigningKey::random(&mut OsRng);
        let spki = SubjectPublicKeyInfoOwned::from_key(*signing_key.verifying_key()).expect("spki");
        let builder = CertificateBuilder::new(
            Profile::Root,
            SerialNumber::from(1u32),
            Validity::from_now(Duration::from_secs(3600)).expect("validity"),
            Name::from_str("CN=ocx-test-ca").expect("name"),
            spki,
            &signing_key,
        )
        .expect("builder");
        builder
            .build::<p256::ecdsa::DerSignature>()
            .expect("build")
            .to_der()
            .expect("der")
    }

    // A CERTIFICATE body that is `SEQUENCE { INTEGER 1 }` — valid DER, never a
    // certificate. A structural byte check accepts it; a real X.509 parse does
    // not, and a chain builder handed it as an anchor would choke later.
    const NOT_A_CERTIFICATE_DER: &[u8] = &[0x30, 0x03, 0x02, 0x01, 0x01];

    #[test]
    fn load_trusted_root_json_rejects_der_that_is_not_a_certificate() {
        let doc = trusted_root_json(NOT_A_CERTIFICATE_DER, true);
        match TrustRoot::load_trusted_root_json(&doc) {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail })) => {
                assert!(
                    detail.contains("invalid X.509 certificate in trust root"),
                    "detail should name the failed parse: {detail}"
                );
            }
            other => panic!("expected TrustRootLoad(PemParseFailed), got: {other:?}"),
        }
    }

    /// Build a `TrustedRoot` JSON document the way `test/sigstore/generate-trusted-root.py`
    /// does, so this test pins the same shape the local stack ships.
    fn trusted_root_json(ca_der: &[u8], with_logs: bool) -> Vec<u8> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        // An arbitrary but well-formed DER SPKI: sigstore's codec treats a key
        // body as opaque bytes, so any blob round-trips.
        let key = b64.encode([0x30u8, 0x03, 0x02, 0x01, 0x01]);
        let log = |url: &str, id: &str| {
            format!(
                r#"{{"baseUrl":"{url}","hashAlgorithm":"SHA2_256","logId":{{"keyId":"{id}"}},
                    "publicKey":{{"rawBytes":"{key}","keyDetails":"PKIX_ECDSA_P256_SHA_256",
                    "validFor":{{"start":"1970-01-01T00:00:00Z"}}}}}}"#
            )
        };
        let logs = if with_logs {
            format!(
                r#""tlogs":[{}],"ctlogs":[{}],"#,
                log("http://localhost:3000", "AQID"),
                log("http://localhost:6962/ocx-test", "BAUG")
            )
        } else {
            String::new()
        };
        format!(
            r#"{{"mediaType":"application/vnd.dev.sigstore.trustedroot+json;version=0.1",
                {logs}
                "certificateAuthorities":[{{"uri":"http://localhost:5555",
                  "certChain":{{"certificates":[{{"rawBytes":"{ca}"}}]}},
                  "validFor":{{"start":"1970-01-01T00:00:00Z"}}}}]}}"#,
            ca = b64.encode(ca_der)
        )
        .into_bytes()
    }

    #[test]
    fn load_trusted_root_json_extracts_fulcio_ctfe_and_rekor_material() {
        let ca = real_cert_der();
        let tr = TrustRoot::load_trusted_root_json(&trusted_root_json(&ca, true)).expect("valid trusted root JSON");
        assert_eq!(tr.der_certs(), &[ca], "the Fulcio anchor must round-trip");
        // The CTFE key is what makes SCT verification possible at all; a trust
        // root that silently harvests zero of them verifies nothing.
        assert_eq!(tr.ctfe_key_map().len(), 1, "one CT log key: {:?}", tr.ctfe_key_map());
        assert_eq!(tr.rekor_key_map().len(), 1, "one Rekor key: {:?}", tr.rekor_key_map());
        let pem = tr.rekor_public_key_pem().expect("Rekor key pinned");
        assert!(pem.contains("BEGIN PUBLIC KEY"), "Rekor key re-encoded to PEM: {pem}");
    }

    #[test]
    fn load_trusted_root_json_without_logs_pins_no_rekor_key() {
        let tr = TrustRoot::load_trusted_root_json(&trusted_root_json(&real_cert_der(), false)).expect("valid");
        assert!(tr.rekor_public_key_pem().is_none(), "no tlogs -> no pinned Rekor key");
        assert!(tr.ctfe_key_map().is_empty(), "no ctlogs -> no CTFE key");
    }

    #[test]
    fn load_trusted_root_json_rejects_no_authorities() {
        match TrustRoot::load_trusted_root_json(br#"{"tlogs":[]}"#) {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCertificateBlocks)) => {}
            other => panic!("expected NoCertificateBlocks, got: {other:?}"),
        }
    }

    #[test]
    fn load_trusted_root_json_rejects_malformed_json() {
        match TrustRoot::load_trusted_root_json(b"not json at all") {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { .. })) => {}
            other => panic!("expected PemParseFailed, got: {other:?}"),
        }
    }

    #[test]
    fn load_trusted_root_json_rejects_a_ca_body_that_is_not_a_certificate() {
        let json = trusted_root_json(&[0x30, 0x03, 0x02, 0x01, 0x01], false);
        match TrustRoot::load_trusted_root_json(&json) {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail })) => {
                assert!(detail.contains("invalid X.509 certificate"), "detail: {detail}");
            }
            other => panic!("expected PemParseFailed for a non-certificate CA body, got: {other:?}"),
        }
    }

    #[test]
    fn from_material_carries_all_three_kinds() {
        let ca = real_cert_der();
        let tr = TrustRoot::from_material(
            vec![ca.clone()],
            BTreeMap::from([("ct".to_string(), vec![1, 2, 3])]),
            BTreeMap::from([("rekor".to_string(), vec![4, 5, 6])]),
        );
        assert_eq!(tr.der_certs(), &[ca]);
        assert_eq!(tr.ctfe_key_map().len(), 1);
        assert!(tr.rekor_public_key_pem().is_some());
    }

    #[test]
    fn the_sigstore_trait_impl_hands_back_what_was_loaded() {
        // The Verifier is constructed from exactly these three accessors, so a
        // trust root that loads material but fails to expose it verifies
        // nothing while looking healthy.
        let ca = real_cert_der();
        let tr = TrustRoot::load_trusted_root_json(&trusted_root_json(&ca, true)).expect("valid");
        assert_eq!(
            SigstoreTrustRootTrait::fulcio_certs(&tr).expect("certs").len(),
            1,
            "the verifier must see the Fulcio anchor"
        );
        assert_eq!(
            SigstoreTrustRootTrait::ctfe_keys(&tr).expect("ctfe").len(),
            1,
            "the verifier must see the CT log key"
        );
    }

    #[test]
    fn every_certificate_in_a_multi_hop_ca_chain_reaches_the_verifier() {
        // Where the intermediate comes from. Sigstore bundle v0.3 carries the
        // leaf alone -- `x509CertificateChain` is deprecated in favour of the
        // singular `certificate` field -- so `sigstore`'s `Verifier::new`
        // builds its `CertificatePool` from `fulcio_certs()` with an empty
        // untrusted-intermediates list. That is only sound because the trusted
        // root's `certChain` names the intermediate alongside the root, and
        // this layer hands back the whole chain.
        //
        // Dropping to the first certificate here would look harmless and would
        // still pass every test against the local stack, which runs Fulcio with
        // `--ca=fileca`: one CA, leaves signed directly by the root, no
        // intermediate to lose. It would then fail against public-good Fulcio,
        // whose leaves are signed by an intermediate.
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let intermediate = real_cert_der();
        let root = real_cert_der();
        assert_ne!(intermediate, root, "the fixture must be two distinct certificates");
        let json = format!(
            r#"{{"mediaType":"application/vnd.dev.sigstore.trustedroot+json;version=0.1",
                "certificateAuthorities":[{{"uri":"http://localhost:5555",
                  "certChain":{{"certificates":[{{"rawBytes":"{a}"}},{{"rawBytes":"{b}"}}]}},
                  "validFor":{{"start":"1970-01-01T00:00:00Z"}}}}]}}"#,
            a = b64.encode(&intermediate),
            b = b64.encode(&root),
        )
        .into_bytes();

        let tr = TrustRoot::load_trusted_root_json(&json).expect("a two-hop CA chain is valid");
        let seen: Vec<Vec<u8>> = SigstoreTrustRootTrait::fulcio_certs(&tr)
            .expect("certs")
            .iter()
            .map(|c| c.as_ref().to_vec())
            .collect();
        assert_eq!(
            seen,
            vec![intermediate, root],
            "both hops must reach the verifier, in the order the document names them",
        );
    }

    /// The rotation case the log-id-aware selector exists for: two Rekor keys
    /// in one trust root. `rekor_public_key_pem` answers with whichever `logId`
    /// sorts first, so a bundle from the *other* log would verify against the
    /// wrong key and report a valid signature as corrupt data (`RekorSetInvalid`
    /// -> exit 65, whose remedy reads "file a bug"). Selecting by log id is what
    /// avoids that; an unknown id still falls back, so a single-log private
    /// stack that writes no meaningful id keeps working.
    #[test]
    fn the_rekor_key_is_selectable_by_log_id_and_falls_back_when_unknown() {
        let tr = TrustRoot::from_material(
            vec![real_cert_der()],
            BTreeMap::new(),
            BTreeMap::from([("aa".to_string(), vec![1, 1, 1]), ("bb".to_string(), vec![2, 2, 2])]),
        );
        let pem_of = |der: &[u8]| pem::encode(&pem::Pem::new("PUBLIC KEY", der.to_vec()));

        assert_eq!(
            tr.rekor_public_key_pem_for("bb").as_deref(),
            Some(pem_of(&[2, 2, 2]).as_str()),
            "the second log's key must be reachable, not shadowed by the first"
        );
        assert_eq!(
            tr.rekor_public_key_pem().as_deref(),
            Some(pem_of(&[1, 1, 1]).as_str()),
            "the log-id-unaware accessor answers with the first key in logId order"
        );
        assert_eq!(
            tr.rekor_public_key_pem_for("unknown").as_deref(),
            Some(pem_of(&[1, 1, 1]).as_str()),
            "an unknown log id falls back rather than refusing"
        );
    }

    /// The blackholed-endpoint case: a TUF endpoint that completes the TCP
    /// handshake and then sends nothing. Without the deadline this is an
    /// unbounded hang with no exit code -- in `ocx package verify`, and through
    /// the auto-verify hook in every covered install on a machine whose trust
    /// cache is cold or expired, turning the fail-closed policy gate into
    /// fail-hung.
    ///
    /// Asserted at [`with_deadline`] rather than through `load_embedded`,
    /// because `sigstore` hardcodes the public-good metadata base URL and builds
    /// the HTTP client itself -- a local listener cannot be substituted, so
    /// `load_embedded` has no reachable failing input short of the real network.
    ///
    /// No clock dependence, so no flake and no multi-second test: the budget is
    /// already spent before the first poll and the fetch is never ready, so
    /// `Elapsed` is decided by poll order alone. That is stricter than a paused
    /// clock would be here, not weaker -- it removes the timer from the
    /// assertion rather than merely fast-forwarding it (ASYNC-13's concern is a
    /// real multi-second sleep in the suite, which this has none of).
    #[tokio::test]
    async fn a_tuf_fetch_that_never_answers_hits_the_deadline() {
        let never_answers = std::future::pending::<Result<(), VerifyErrorKind>>();
        match with_deadline(Duration::ZERO, never_answers).await {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::TufFetchTimeout)) => {}
            other => panic!("expected TufFetchTimeout, got: {other:?}"),
        }
    }

    /// The other half of the proof: at the real budget, a fetch that is merely
    /// not ready on its first poll still passes its value through. Progress here
    /// comes from the scheduler, never from the clock, so for this to time out
    /// the full 60s would have to elapse across a handful of yields -- which is
    /// why it can carry [`TUF_FETCH_DEADLINE`] itself without becoming
    /// load-sensitive.
    #[tokio::test]
    async fn a_tuf_fetch_that_answers_inside_the_deadline_is_not_cut_short() {
        let answers_after_yielding = async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            Ok::<_, VerifyErrorKind>(7u8)
        };
        let fetched = with_deadline(TUF_FETCH_DEADLINE, answers_after_yielding)
            .await
            .expect("a fetch that answers inside the deadline must pass through");
        assert_eq!(fetched, 7, "the fetched value must survive the wrapper");
    }
}
