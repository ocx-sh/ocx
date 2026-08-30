// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Reading cosign's `sha256-<hex>.att` sidecar — the attestation half of
//! spec §WP5, and the *only* spelling `.att` has.
//!
//! # Why there is no `.att` artifact type to discover by
//!
//! Measured against cosign v3.1.1, not inferred (`generate.py`'s
//! `_capture_attestation_sidecar` and `ATT_CERTIFICATE_GAP` record the
//! commands):
//!
//! * `cosign attest` against a Referrers-API registry writes an OCI 1.1
//!   referrer whose `artifactType` is
//!   [`SIGSTORE_BUNDLE_V03`](crate::oci::referrer::media_types::SIGSTORE_BUNDLE_V03) —
//!   the **same** type a signature referrer carries. An attestation is told from
//!   a signature by the `dev.sigstore.bundle.content: dsse-envelope` annotation
//!   and its `predicateType` sibling, never by a distinct artifact type. That
//!   shape is already discovered by [`super::pipeline`]'s bundle scan.
//! * Against a registry without one it writes the `sha256-<hex>` fallback
//!   *index*, again with `SIGSTORE_BUNDLE_V03` children — not a `.att` tag.
//! * `--registry-referrers-mode` is not a flag on `attest` at all (it exists on
//!   `attach sbom`, which is where
//!   [`COSIGN_SBOM_ARTIFACT_TYPE`](crate::oci::referrer::media_types::COSIGN_SBOM_ARTIFACT_TYPE)
//!   was captured), so the
//!   `application/vnd.dev.cosign.artifact.%s.v1+json` template the binary
//!   carries is never instantiated for an attestation.
//! * The `.att` manifest cosign writes carries **neither** `artifactType`
//!   **nor** `subject`, so it is invisible to the Referrers API by
//!   construction. `attestation_sidecar_key_manifest.json` pins both absences.
//!
//! So `.att` is a tag-only shape, and this module is its reader. G1 froze the
//! artifact-type set completely after all: there is no third constant.
//!
//! # A different layer shape, the same trust gate
//!
//! An `.att` layer is a [`DSSE_ENVELOPE_MEDIA_TYPE`] envelope, not a
//! [`SIMPLESIGNING_MEDIA_TYPE`](crate::oci::referrer::media_types::SIMPLESIGNING_MEDIA_TYPE)
//! claim — which is why [`super::simplesigning_read::read_sidecar_manifest`]
//! returns an empty scan when aimed at this tag, and why this module exists
//! beside it rather than inside it. What it does **not** do is parse the
//! envelope a second time: the structural half is
//! [`super::dsse::verify_envelope`], the same function the bundle path runs, and
//! the keyless gate is the same [`Verifier`] both other doors use.
//!
//! Three shapes, exactly as the simplesigning sidecar has, and none of them is
//! malformed input:
//!
//! * **Key mode** — no certificate annotation. The signature lives *inside* the
//!   envelope, so nothing is carried in annotations at all; it is checked over
//!   the PAE against a key the trust policy named
//!   ([`identity::matching_key_policies`]), which is the same arm
//!   [`super::pipeline`] runs for a key-mode bundle. The shape cosign v3.1.1's
//!   `attach attestation` writes, and the shape the golden fixture pins.
//! * **Keyless** — [`ANNOTATION_COSIGN_CERTIFICATE`] carries the Fulcio leaf and
//!   [`ANNOTATION_COSIGN_BUNDLE`] the offline Rekor bundle; the shape cosign 2.x's
//!   `attest` wrote, and the shape OCX's own writer emits
//!   (`oci::sign::simplesigning_write`'s `SidecarLayer::attestation`, which sets
//!   both annotations). The identical keyless gate runs, and it is six steps
//!   rather than four: chain to the trust root, embedded SCT, DSSE signature over
//!   the PAE, SAN + OIDC issuer, the trust policy's `builder` pin (#103), the
//!   logged body bound to *this* envelope, and the certificate's validity window
//!   against the entry's own `integratedTime`.
//! * **Keyless with no transparency material** — **refused**
//!   ([`VerifyErrorKind::SignatureInvalid`]) unless the caller passes
//!   `--allow-unlogged-signature`; see [`verify_keyless`] for the ordering and
//!   the reasoning, which is its sibling's verbatim.
//!
//! # Transparency-log evidence is the keyless gate
//!
//! A keyless signature's certificate lives about ten minutes. *When* it was used
//! is therefore the only thing separating a live signature from a stale
//! certificate replayed for ever, and the transparency log is the only place
//! that answer comes from. So on the keyless arm the [`ANNOTATION_COSIGN_BUNDLE`]
//! annotation is **required and checked**, by the sibling's own
//! [`logged_entry`] — payload-agnostic, and called from here rather than copied:
//! its Signed Entry Timestamp is verified against the log's own public key, its
//! logged `dsse:0.0.1` body is bound to this envelope
//! ([`dsse::verify_tlog_binding`], the identical call the bundle path makes),
//! and only then does its `integratedTime` become the instant both this module
//! and `sigstore` judge the certificate's window against.
//!
//! `signed_at` and `rekor_log_index` are reported from that entry, and only from
//! it — never from the entry [`sidecar_bundle`] synthesises so `sigstore`'s
//! `CheckedBundle` has the one it demands.
//!
//! # Gaps, carried from `simplesigning_read`
//!
//! * **The key arm does not read [`ANNOTATION_COSIGN_BUNDLE`].** `ocx package
//!   sign --key` uploads to Rekor only when asked (D10), and a key signature's
//!   trust story is a committed public key rather than a signing instant. The
//!   keyless arm is where the entry is *required*, which is where reading it has
//!   to be right.
//! * **No online Rekor lookup.** Evidence comes from the offline annotation or
//!   not at all.
//! * **The keyless `.att` shape is not capturable from cosign v3.1.1.**
//!   `attach attestation` takes no `--certificate`, and `attest` no longer
//!   writes the tag. The keyless arm is covered by unit tests built from the
//!   cosign-authored `keyless_bundle.json` instead — cosign's own certificate,
//!   cosign's own envelope and that bundle's own `dsse:0.0.1` log entry,
//!   packaged into the layer shape measured above.
//!
//! [`DSSE_ENVELOPE_MEDIA_TYPE`]: crate::oci::referrer::media_types::DSSE_ENVELOPE_MEDIA_TYPE
//! [`ANNOTATION_COSIGN_CERTIFICATE`]: crate::oci::referrer::media_types::ANNOTATION_COSIGN_CERTIFICATE
//! [`ANNOTATION_COSIGN_BUNDLE`]: crate::oci::referrer::media_types::ANNOTATION_COSIGN_BUNDLE
//! [`logged_entry`]: super::simplesigning_read::logged_entry

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, VerificationMaterial, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::common::v1::{X509Certificate, X509CertificateChain};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{InclusionPromise, TransparencyLogEntry};
use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};
use x509_cert::der::EncodePem as _;

use super::discovery::DiscoveryMethod;
use super::dsse::{self, VerifiedAttestation, VerifiedEnvelope};
use super::error::VerifyErrorKind;
use super::identity::{self, matching_policies, oidc_issuer, parse_certificate, subject_identity};
use super::pipeline::{
    ACCEPTED_MANIFEST_TYPES, MAX_REFERRER_MANIFEST_BYTES, PolicyDeferredToOcx, RefusedCandidate, ScanStop,
    VerifiedSignature, VerifyResult, map_client_error, map_verification_error, pull_blob_capped,
};
use super::signing_instant::SigningInstant;
use super::simplesigning_read::{
    LoggedEntry, SIGSTORE_BUNDLE_V01_MEDIA_TYPE, SidecarKind, SidecarVerification, layer_certificate, layer_chain,
    logged_entry,
};
use super::tlog;
use crate::oci::attest::dsse::{DsseEnvelope, pae};
use crate::oci::attest::predicate::PredicateType;
use crate::oci::attest::{DSSE_PAYLOAD_TYPE, MAX_ATTESTATION_CANDIDATES, MAX_ATTESTATION_ENVELOPE_BYTES};
use crate::oci::client::error::ClientError;
use crate::oci::client::{OciTransport, sibling_tag_reference};
use crate::oci::referrer::media_types::DSSE_ENVELOPE_MEDIA_TYPE;
use crate::oci::sign::{KeyBackendKind, SignatureFormat};
use crate::oci::{Algorithm, Descriptor, Digest, ImageManifest, native};

/// What one `.att` sidecar yielded.
///
/// Matches and refusals travel together for the reason
/// [`super::pipeline::AttestationScan`] gives: one malformed layer must not be
/// able to hide every valid attestation beside it.
#[derive(Debug, Default)]
pub(super) struct AttestationSidecarScan {
    /// Every DSSE layer that verified, in manifest order.
    pub matches: Vec<SidecarAttestation>,
    /// Every DSSE layer that was examined and refused, in manifest order.
    /// `referrer_digest` is the **layer** digest: one layer is one attestation.
    pub refused: Vec<RefusedCandidate>,
    /// Bytes actually read — the manifest plus every envelope pulled — so the
    /// caller charges its cross-candidate budget from reads rather than from
    /// declared sizes, exactly as the bundle loop does.
    pub bytes_read: u64,
    /// The bound that stopped the walk short of the sidecar's DSSE layers, when
    /// one did.
    ///
    /// The caller records it on the shared budget, which is what makes
    /// `finish_scan`'s fail-closed attestation arm fire: a truncated scan has
    /// looked at fewer attestations than the sidecar carries, so the partial
    /// list below is **not** an answer to "every attestation on this subject".
    pub stop: Option<ScanStop>,
}

/// One verified `.att` layer, in the shape the caller's dedup pass takes.
///
/// [`VerifiedSignature`] rather than a bare [`VerifyResult`], because D6's
/// dedup key falls back on the signature bytes whenever no transparency-log
/// index is credited — which is every key-mode `.att` layer, the shape cosign
/// v3.1.1 actually writes. The pair mirrors the bundle path's
/// `CandidateOutcome::Verified` exactly, so both doors dedup against one
/// `SignatureKey` set.
#[derive(Debug)]
pub(super) struct SidecarAttestation {
    /// The verification facts, plus the bytes the dedup key falls back on.
    pub verified: VerifiedSignature,
    /// The attestation this layer carried.
    pub attestation: VerifiedAttestation,
}

/// Fetch the `sha256-<hex>.att` tag and verify every DSSE layer it carries.
///
/// `Ok(None)` means the tag does not exist — "no legacy attestation", the
/// overwhelmingly common case, never an error.
///
/// `predicate_type` narrows exactly as it does on the bundle path: `None`
/// accepts any signed predicateType, `Some` skips a layer whose signed type is
/// something else without recording a refusal (S-017).
///
/// `byte_budget` is what the caller has left of its cross-candidate allowance.
/// A sidecar's layers are read against it and the scan stops when it is gone —
/// without that, `MAX_ATTESTATION_CANDIDATES` envelopes at
/// [`MAX_ATTESTATION_ENVELOPE_BYTES`] each would be an unbounded read behind a
/// single candidate slot (CWE-400).
///
/// # Errors
///
/// [`VerifyErrorKind`] when the registry fails for any reason other than a
/// missing manifest, or when the sidecar manifest is over-cap or does not
/// parse. A *layer* failure is never an error here — it lands in
/// [`AttestationSidecarScan::refused`].
pub(super) async fn read_attestation_sidecar_tag(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
    subject_bytes: &[u8],
    verify: &SidecarVerification<'_>,
    predicate_type: Option<&PredicateType>,
    byte_budget: u64,
) -> Result<Option<AttestationSidecarScan>, VerifyErrorKind> {
    let target = sibling_tag_reference(
        image,
        super::simplesigning_read::sidecar_tag(subject_digest, SidecarKind::Attestation),
    );
    let bytes = match transport.pull_manifest_raw(&target, ACCEPTED_MANIFEST_TYPES).await {
        Ok((bytes, _digest)) => bytes,
        Err(ClientError::ManifestNotFound(_)) => return Ok(None),
        Err(other) => return Err(map_client_error(other)),
    };
    if bytes.len() as u64 > MAX_REFERRER_MANIFEST_BYTES {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    read_attestation_sidecar_manifest(
        transport,
        image,
        &bytes,
        subject_digest,
        subject_bytes,
        verify,
        predicate_type,
        byte_budget.saturating_sub(bytes.len() as u64),
    )
    .await
    .map(|mut scan| {
        scan.bytes_read = scan.bytes_read.saturating_add(bytes.len() as u64);
        Some(scan)
    })
}

/// Verify every DSSE layer of an already-fetched `.att` manifest.
///
/// A layer whose media type is not [`DSSE_ENVELOPE_MEDIA_TYPE`] is **skipped**,
/// not refused — the same tolerance the simplesigning reader applies for the
/// same reason: a sidecar legitimately carries other layers.
///
/// # Errors
///
/// [`VerifyErrorKind::BundleParseFailed`] when the manifest does not parse as an
/// OCI image manifest.
#[expect(
    clippy::too_many_arguments,
    reason = "one sidecar, its two subjects, and the run-scoped material"
)]
async fn read_attestation_sidecar_manifest(
    transport: &dyn OciTransport,
    image: &native::Reference,
    manifest_bytes: &[u8],
    subject_digest: &Digest,
    subject_bytes: &[u8],
    verify: &SidecarVerification<'_>,
    predicate_type: Option<&PredicateType>,
    byte_budget: u64,
) -> Result<AttestationSidecarScan, VerifyErrorKind> {
    let manifest: ImageManifest =
        serde_json::from_slice(manifest_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

    let mut scan = AttestationSidecarScan::default();
    let layers: Vec<&Descriptor> = manifest
        .layers
        .iter()
        .filter(|layer| layer.media_type == DSSE_ENVELOPE_MEDIA_TYPE)
        .collect();
    if layers.len() > MAX_ATTESTATION_CANDIDATES {
        // Recorded, not merely logged. A debug line is invisible to the caller,
        // and the caller is the only party that can refuse: collect-all is the
        // mode that cares, and a truncated walk has looked at fewer
        // attestations than the sidecar carries. Left as a log, a manifest
        // listing one genuine layer descriptor 32 times and the real
        // attestation as the 33rd exits 0 having never fetched it.
        tracing::debug!(
            "`.att` sidecar carries {} DSSE layers; examining the first {MAX_ATTESTATION_CANDIDATES}",
            layers.len()
        );
        scan.stop = Some(ScanStop::CandidateCap);
    }
    for layer in layers.into_iter().take(MAX_ATTESTATION_CANDIDATES) {
        if scan.bytes_read >= byte_budget {
            // The other truncation, reported the same way and for the same
            // reason: the partial result carries the bytes that were spent
            // getting it, and the bound that ended it.
            tracing::debug!("`.att` sidecar scan stopped: the cross-candidate byte budget is spent");
            scan.stop.get_or_insert(ScanStop::ByteBudget);
            break;
        }
        let envelope_bytes = match pull_envelope(transport, image, layer, &mut scan.bytes_read).await {
            Ok(bytes) => bytes,
            Err(kind) => {
                scan.refused.push(RefusedCandidate {
                    referrer_digest: layer.digest.clone(),
                    reason: kind,
                });
                continue;
            }
        };
        match verify_layer(
            layer,
            &envelope_bytes,
            subject_digest,
            subject_bytes,
            verify,
            predicate_type,
        )
        .await
        {
            Ok(Some(found)) => scan.matches.push(found),
            // A narrowing miss: this layer is sound, it simply is not the
            // document that was asked for. Records no refusal, so a scan that
            // finds only these still reports not-found.
            Ok(None) => {}
            Err(reason) => scan.refused.push(RefusedCandidate {
                referrer_digest: layer.digest.clone(),
                reason,
            }),
        }
    }
    Ok(scan)
}

/// Pull one DSSE envelope under [`MAX_ATTESTATION_ENVELOPE_BYTES`], charging
/// `bytes_read` for what the attempt actually cost.
///
/// The declared descriptor size is untrusted, so it is only a cheap pre-fetch
/// reject; the read itself is bounded independently (CWE-400).
///
/// # What each outcome costs
///
/// The two refusals above the fetch are decided from the descriptor alone,
/// before a connection is opened, so they are charged **nothing** — the shape
/// [`super::pipeline`] uses at both of its own capped pulls. Charging the cap for
/// them would be CWE-400 in reverse: two zero-cost descriptors (a negative size,
/// an unparseable digest) would exhaust a 64 MiB run budget for free and let a
/// hostile registry truncate the scan without serving a byte. A refusal *past*
/// the fetch is charged the cap, because the bounded read stops at cap + 1
/// rather than at zero.
async fn pull_envelope(
    transport: &dyn OciTransport,
    image: &native::Reference,
    layer: &Descriptor,
    bytes_read: &mut u64,
) -> Result<Vec<u8>, VerifyErrorKind> {
    if layer.size < 0 || layer.size as usize > MAX_ATTESTATION_ENVELOPE_BYTES {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    let digest = Digest::try_from(layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    match pull_blob_capped(transport, image, &digest, MAX_ATTESTATION_ENVELOPE_BYTES).await {
        Ok(bytes) => {
            *bytes_read = bytes_read.saturating_add(bytes.len() as u64);
            Ok(bytes)
        }
        Err(kind) => {
            *bytes_read = bytes_read.saturating_add(MAX_ATTESTATION_ENVELOPE_BYTES as u64);
            Err(kind)
        }
    }
}

/// Verify one `.att` layer against `subject_digest`.
///
/// `Ok(None)` is the narrowing miss — a sound attestation of another
/// predicateType — and is the one outcome that is neither a match nor a defect.
async fn verify_layer(
    layer: &Descriptor,
    envelope_bytes: &[u8],
    subject_digest: &Digest,
    subject_bytes: &[u8],
    verify: &SidecarVerification<'_>,
    predicate_type: Option<&PredicateType>,
) -> Result<Option<SidecarAttestation>, VerifyErrorKind> {
    let layer_digest = Digest::try_from(layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

    // Rows 3, 8 and 16 — the payload-type check, the one-signature rule and the
    // decoded-payload cap — all belong to this parser, not to a second copy of
    // them here.
    let envelope = DsseEnvelope::parse(envelope_bytes)?;
    // Unreachable rather than a check: `DsseEnvelope::parse` refuses every
    // signature count but exactly one (`MultipleSignatures { count }`), so this
    // is the defensive floor the bundle path's key arm keeps for the same
    // reason — there is no second source of signature bytes to fall back on.
    let signature = envelope
        .signatures
        .first()
        .ok_or(VerifyErrorKind::SignatureInvalid)?
        .sig
        .clone();

    // OCX's own structural checks run BEFORE anything else, exactly as the
    // simplesigning door's `check_claim` does, and for two reasons neither of
    // which is style. A layer that should be an `Ok(None)` narrowing miss under
    // `--type X` must not be able to land in `refused` instead: a registry
    // could otherwise flip `attestation_not_found` into `bundle_parse_failed`
    // or `rekor_set_invalid` by corrupting the annotation of a document nobody
    // asked for (S-017). And a layer bound to *another* subject must not cost a
    // Rekor public-key resolution before it is refused.
    //
    // The bundle built here carries no verification material — none is needed,
    // `verify_envelope` reads only the envelope — so the keyless work below
    // rebuilds it once the material is in hand.
    let structural = sidecar_bundle(&envelope, None, &signature)?;
    let verified = match dsse::verify_envelope(&structural, subject_digest, predicate_type) {
        Ok(verified) => verified,
        Err(VerifyErrorKind::PredicateTypeMismatch { .. }) if predicate_type.is_some() => return Ok(None),
        Err(kind) => return Err(kind),
    };

    let signer = match layer_certificate(layer)? {
        // The keyless material is gathered BEFORE the bundle it travels in is
        // built, because the entry that bundle carries must hold the log's own
        // `integratedTime`: `sigstore` anchors both its chain build and its
        // certificate-expiry check on that field, so the certificate's own
        // `notBefore` there would have the library judge the certificate
        // against its own answer (`super::signing_instant`).
        Some(leaf_der) => {
            let keyless = gather_keyless(layer, leaf_der, verify).await?;
            let bundle = sidecar_bundle(&envelope, Some(&keyless), &signature)?;
            verify_keyless(keyless, subject_bytes, bundle, verify, &verified).await?
        }
        // Key mode. The PAE, never the bare payload: a signature checked over
        // the payload alone is forgeable across payload types, which is DSSE's
        // whole reason for the encoding. `DsseEnvelope::parse` already refused
        // any `payloadType` but this one, so the constant is the envelope's own
        // declared type and not an assumption.
        None => {
            let pae = pae(DSSE_PAYLOAD_TYPE, &verified.attestation.payload);
            let matched = identity::matching_key_policies(&pae, &signature, verify.policies)?;
            dsse::enforce_builder_pin(&matched, &verified.attestation)?;
            VerifiedSigner {
                key_backend: KeyBackendKind::File,
                certificate_identity: None,
                certificate_oidc_issuer: None,
                logged: None,
            }
        }
    };

    Ok(Some(SidecarAttestation {
        verified: VerifiedSignature {
            result: VerifyResult {
                subject_digest: subject_digest.clone(),
                referrer_digest: layer_digest,
                key_backend: signer.key_backend,
                certificate_identity: signer.certificate_identity,
                certificate_oidc_issuer: signer.certificate_oidc_issuer,
                // Only ever from an entry whose SET verified against the log's own
                // key and whose logged body was bound to this envelope. `None` is
                // the key-mode arm and the `--allow-unlogged-signature` arm, where
                // nothing proved a signing time and reporting one would invent it.
                signed_at: signer
                    .logged
                    .as_ref()
                    .and_then(|entry| u64::try_from(entry.integrated_time()).ok()),
                signature_format: SignatureFormat::Simplesigning,
                discovery_method: DiscoveryMethod::SidecarTag,
                // The *annotation's* index, and only after `logged_entry` verified
                // it. The entry `sidecar_bundle` synthesises for `sigstore`'s
                // `CheckedBundle` carries `log_index: 0` and is never checked, so
                // its index must never reach here — it would name a log position
                // nothing proved and key the caller's dedup on a constant.
                rekor_log_index: signer.logged.as_ref().map(LoggedEntry::log_index),
            },
            // The DSSE signature this layer carried, kept beside the result so
            // the caller's dedup key has the material it falls back on whenever
            // no transparency-log index was credited — the key-mode arm, which
            // is every `.att` sidecar cosign v3.1.1 writes.
            signature,
        },
        attestation: verified.attestation,
    }))
}

/// The facts the two key models establish differently — the `.att` twin of the
/// split both other doors keep, so a swap of two adjacent `Option<String>`s
/// cannot type-check silently.
#[derive(Debug)]
struct VerifiedSigner {
    key_backend: KeyBackendKind,
    certificate_identity: Option<String>,
    certificate_oidc_issuer: Option<String>,
    /// The verified transparency-log entry, when the layer carried one.
    logged: Option<LoggedEntry>,
}

/// Everything the keyless arm needs, read once and in one place.
///
/// It exists because the transparency entry has to be in hand *before* the
/// bundle is built — [`Self::anchor`] is what the synthesised entry carries —
/// while the leaf, the chain and the parsed certificate are all read from the
/// same annotations in the same pass. Threading four values through
/// [`sidecar_bundle`] and [`verify_keyless`] separately is the missing type
/// ARCH-01 names.
struct KeylessMaterial {
    /// The leaf certificate annotation, as DER.
    leaf_der: Vec<u8>,
    /// The same leaf, parsed once: the window check, the identity read-back and
    /// the bundle's PEM all use it, so a second parse could only disagree.
    cert: x509_cert::Certificate,
    /// The intermediate chain annotation as DER, empty when absent.
    chain_ders: Vec<Vec<u8>>,
    /// The verified transparency-log entry, when the layer carried one.
    logged: Option<LoggedEntry>,
    /// The instant the synthesised entry carries, and therefore the one
    /// `sigstore` anchors its chain build and its certificate-expiry check on:
    /// the entry's own `integratedTime` whenever one exists.
    ///
    /// Under `--allow-unlogged-signature` there is no entry, `sigstore` still
    /// demands one to hold a bundle together, and the leaf's own `notBefore`
    /// stands in — which makes that library check vacuous. That vacuity is
    /// precisely what the opt-out buys, is reachable no other way, and is why
    /// this is a bare `i64` rather than a [`SigningInstant`]: the type exists to
    /// stop a value nothing proved from being *called* a signing instant.
    anchor: i64,
}

/// Read one keyless `.att` layer's annotations, and its log entry with them.
///
/// # Errors
///
/// [`VerifyErrorKind::CertChainInvalid`] when the leaf or the chain annotation
/// does not parse, plus everything [`logged_entry`] refuses: a malformed
/// annotation, a SET that does not hold, or a log key that can be neither
/// pinned nor fetched.
async fn gather_keyless(
    layer: &Descriptor,
    leaf_der: Vec<u8>,
    verify: &SidecarVerification<'_>,
) -> Result<KeylessMaterial, VerifyErrorKind> {
    let cert = parse_certificate(&leaf_der)?;
    let chain_ders = layer_chain(layer)?;
    let logged = logged_entry(layer, verify).await?;
    // Read the sibling's identical fold: the entry's own instant whenever one
    // exists, and only otherwise the leaf's `notBefore`.
    let anchor = logged.as_ref().map_or_else(
        || i64::try_from(cert.tbs_certificate.validity.not_before.to_unix_duration().as_secs()).unwrap_or(i64::MAX),
        LoggedEntry::integrated_time,
    );
    Ok(KeylessMaterial {
        leaf_der,
        cert,
        chain_ders,
        logged,
        anchor,
    })
}

/// The full keyless gate over an annotation certificate.
///
/// Chain, SCT and the DSSE signature are `sigstore`'s, reached through the same
/// [`Verifier`](sigstore::bundle::verify::Verifier) the bundle path uses;
/// identity and issuer are [`matching_policies`]; the `builder` pin is
/// [`dsse::enforce_builder_pin`], the same call both bundle-path arms and this
/// module's own key arm make; the logged body is bound by
/// [`dsse::verify_tlog_binding`]; and the certificate-validity window is
/// [`tlog::verify_integrated_time_within_certificate`], anchored on the log
/// entry's `integratedTime`. Nothing is relaxed because the material arrived in
/// annotations rather than in a bundle blob.
///
/// # Transparency-log evidence is required, and the ordering says why
///
/// `logged` is `None` when the layer carried no [`ANNOTATION_COSIGN_BUNDLE`]
/// annotation. A keyless signature with nothing in the transparency log has no
/// provable signing instant: its Fulcio leaf lived about ten minutes and expired
/// long before anyone verifies, so *when* it was used is the only question that
/// separates a live signature from a stale certificate replayed for ever.
/// Without an entry the refusal is [`VerifyErrorKind::SignatureInvalid`] — the
/// signature could not be established, which is what 65 says — unless
/// `allow_unlogged` was passed.
///
/// That refusal comes **last**, after chain, SCT, signature, identity and the
/// `builder` pin, and deliberately: an attestation signed by the wrong identity
/// must still report `identity_mismatch`, which is the more actionable verdict
/// and the one an operator can fix. Only an artifact that is right in every
/// other respect gets told the thing that is missing is the log entry.
///
/// The ordering claim is about the **missing-entry** refusal and only it. A
/// layer carrying an annotation that is malformed, whose base64 or SET does not
/// hold, or whose log key cannot be resolved is judged by [`logged_entry`] in
/// [`gather_keyless`], before this function is entered at all.
///
/// [`ANNOTATION_COSIGN_BUNDLE`]: crate::oci::referrer::media_types::ANNOTATION_COSIGN_BUNDLE
async fn verify_keyless(
    keyless: KeylessMaterial,
    subject_bytes: &[u8],
    bundle: Bundle,
    verify: &SidecarVerification<'_>,
    verified: &VerifiedEnvelope,
) -> Result<VerifiedSigner, VerifyErrorKind> {
    let KeylessMaterial {
        leaf_der, cert, logged, ..
    } = keyless;

    // `verify` over the subject bytes, not `verify_digest`: the digest variant
    // takes `sigstore`'s own `sha2::Sha256` value and that crate resolves a
    // different `sha2` semver line than `ocx_lib` does. Handing it the subject
    // slice has it compute the same SHA-256 internally — which for DSSE content
    // is compared against the Statement's own subject digest, closing the
    // cross-subject splice a second time.
    //
    // `offline: true` because the entry, when there is one, arrived in the
    // annotation and was checked by `logged_entry` before this call; see
    // `sidecar_bundle` for what the synthesised entry does and does not decide.
    if let Err(error) = verify
        .verifier
        .verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)
        .await
    {
        return Err(map_verification_error(error));
    }

    // Identity + issuer against the resolved trust policies (ANY-of), read back
    // off the leaf that just passed the chain, the SCT and the signature. The
    // matched subset, not a boolean: the `builder` pin below is ANDed within a
    // policy and ORed across the set, so it is decided from the policies this
    // certificate actually satisfied.
    let matched = matching_policies(&leaf_der, verify.policies)?;

    // #103, on the arm that carries SLSA provenance most often. Discarding the
    // matched set here — which this module did — left a `builder`-pinned trust
    // policy silently unenforced for a keyless `.att`, while the key arm beside
    // it and both bundle-path arms enforced it.
    dsse::enforce_builder_pin(&matched, &verified.attestation)?;

    match logged.as_ref() {
        Some(entry) => {
            // The binding, and it runs HERE rather than beside the SET check
            // that produced this entry. Both orders refuse a tampered
            // signature — every check has to pass — but only this one refuses
            // it as `signature_invalid`. Checked beside the SET, a flipped
            // envelope signature would report a tlog binding mismatch, which
            // points at the log for a fault that is entirely in the bytes
            // beside it. What reaches this line is a signature that already
            // verified under a chained, SCT-checked, identity-matched
            // certificate, so a mismatch here really is a spliced entry.
            //
            // The bundle path's own binder, not a second copy: the body is the
            // same `dsse:0.0.1` shape `oci::sign::rekor`'s `dsse_proposal_body`
            // uploads, and its `verifier` field is deliberately not compared —
            // that would mean re-deriving byte-for-byte the PEM cosign uploaded,
            // which no round trip guarantees, and it buys nothing (the
            // certificate is independently chained, SCT-checked and
            // identity-matched, and the Statement is bound to the subject).
            dsse::verify_tlog_binding(entry.body(), &verified.attestation.payload, &verified.signatures)?;
            // Row 13 (CVE-2024-55655), re-asserted here as it is on both other
            // doors, and now against real evidence: the entry's
            // `integratedTime`, SET-checked and, one line above, bound to this
            // envelope.
            tlog::verify_integrated_time_within_certificate(
                SigningInstant::TransparencyLog(entry.integrated_time()),
                &cert,
            )?;
        }
        // The opt-out. The window check is SKIPPED rather than fed the
        // certificate's own `notBefore`: a check that asks the certificate when
        // it was valid and then judges the certificate against that answer can
        // never fail, and a call that can never fail reads as a gate while being
        // none. The caller said they accept a signature nothing timestamps.
        None if verify.allow_unlogged => {
            tracing::debug!(
                "accepting a keyless `.att` sidecar with no transparency-log evidence (--allow-unlogged-signature)"
            );
        }
        None => return Err(VerifyErrorKind::SignatureInvalid),
    }

    Ok(VerifiedSigner {
        key_backend: KeyBackendKind::Keyless,
        certificate_identity: subject_identity(&cert),
        certificate_oidc_issuer: oidc_issuer(&cert),
        logged,
    })
}

/// The Sigstore bundle an `.att` layer's material describes.
///
/// Built so the keyless gate is **the same code** the bundle path runs rather
/// than a second implementation of chain building and SCT verification, exactly
/// as [`super::simplesigning_read`]'s twin is — and so the structural half
/// ([`dsse::verify_envelope`]) has the one input shape it takes.
///
/// Under a key there is no verification material to build: nothing on that arm
/// is handed to `sigstore`, and the bundle exists only to carry the envelope to
/// the structural check.
///
/// # The transparency-log entry, and what it does and does not decide
///
/// `sigstore`'s `CheckedBundle` requires exactly one entry, so one is supplied:
/// the Rekor `dsse` v0.0.1 body this envelope *would* be logged under, derived
/// from the envelope and the certificate. Its *body* is not evidence and is
/// never treated as any — the inclusion promise is empty, `sigstore` 0.14
/// verifies neither the SET nor the Merkle proof, and its only consumer is that
/// library's consistency comparison against the same four values it re-derives.
/// The **real** entry, the one the layer's `dev.sigstore.cosign/bundle`
/// annotation carried, is SET-checked by [`logged_entry`] and bound to this
/// envelope by [`dsse::verify_tlog_binding`]; keeping the synthesised body here
/// rather than substituting the logged one is deliberate, because feeding
/// `sigstore` the log's body would compare a `verifier` PEM byte-for-byte
/// against a round-tripped one, which nothing guarantees.
///
/// What the synthesised entry *does* decide is time. [`KeylessMaterial::anchor`]
/// becomes its `integrated_time`, and `sigstore` anchors both its chain build
/// and its certificate-expiry check on that field — so the anchor is the logged
/// `integratedTime` whenever there is one, and the leaf's own `notBefore` only
/// under `--allow-unlogged-signature`, where that library check is vacuous by
/// construction. Passing `notBefore` unconditionally, as this module once did,
/// made it vacuous always: a ten-minute Fulcio leaf verified for ever.
fn sidecar_bundle(
    envelope: &DsseEnvelope,
    keyless: Option<&KeylessMaterial>,
    signature: &[u8],
) -> Result<Bundle, VerifyErrorKind> {
    let proto = Envelope {
        payload: envelope.payload.clone(),
        payload_type: envelope.payload_type.clone(),
        signatures: envelope
            .signatures
            .iter()
            .map(|signature| Signature {
                sig: signature.sig.clone(),
                keyid: signature.keyid.clone(),
            })
            .collect(),
    };

    let verification_material = match keyless {
        None => None,
        Some(keyless) => {
            let leaf_pem = keyless
                .cert
                .to_pem(x509_cert::der::pem::LineEnding::LF)
                .map_err(|_| VerifyErrorKind::CertChainInvalid)?;
            // The exact bytes `CheckedBundle` hashes for `envelopeHash`:
            // `serde_json::to_vec` over the same protobuf value it will
            // serialize itself, so the two agree by construction rather than by
            // a transcribed field order.
            let envelope_json = serde_json::to_vec(&proto).map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;
            let body = serde_json::json!({
                "kind": "dsse",
                "apiVersion": "0.0.1",
                "spec": {
                    "envelopeHash": {
                        "algorithm": "sha256",
                        "value": Algorithm::Sha256.hash(&envelope_json).hex(),
                    },
                    "payloadHash": {
                        "algorithm": "sha256",
                        "value": Algorithm::Sha256.hash(&envelope.payload).hex(),
                    },
                    "signatures": [{
                        "signature": BASE64.encode(signature),
                        "verifier": BASE64.encode(leaf_pem.as_bytes()),
                    }],
                },
            });
            let canonicalized_body =
                serde_json_canonicalizer::to_vec(&body).map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;

            let mut certificates = vec![X509Certificate {
                raw_bytes: keyless.leaf_der.clone(),
            }];
            certificates.extend(keyless.chain_ders.iter().map(|raw_bytes| X509Certificate {
                raw_bytes: raw_bytes.clone(),
            }));

            Some(VerificationMaterial {
                timestamp_verification_data: None,
                tlog_entries: vec![TransparencyLogEntry {
                    log_index: 0,
                    log_id: None,
                    kind_version: None,
                    integrated_time: keyless.anchor,
                    inclusion_promise: Some(InclusionPromise {
                        signed_entry_timestamp: Vec::new(),
                    }),
                    inclusion_proof: None,
                    canonicalized_body,
                }],
                content: Some(verification_material::Content::X509CertificateChain(
                    X509CertificateChain { certificates },
                )),
            })
        }
    };

    Ok(Bundle {
        // The 0.1 profile: the one whose structural check an entry with no
        // Merkle proof satisfies, which is the shape a sidecar has.
        media_type: SIGSTORE_BUNDLE_V01_MEDIA_TYPE.to_owned(),
        verification_material,
        content: Some(bundle::Content::DsseEnvelope(proto)),
    })
}

#[cfg(test)]
mod tests {
    //! Verified against **committed cosign v3.1.1 output**, the precedent
    //! `simplesigning_read`'s own tests set: `include_str!`/`include_bytes!` so
    //! a moved fixture is a compile error, and the local trust root so every
    //! keyless assertion runs offline with no container.
    use super::*;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::referrer::media_types::{ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE};
    use crate::oci::verify::trust_root::TrustRoot;
    use crate::trust::{CompiledKeyless, CompiledPolicy, IdentityRule, PolicyBackend};
    use sigstore::bundle::verify::Verifier;
    use sigstore::rekor::apis::configuration::Configuration as RekorConfiguration;
    use url::Url;

    /// The `.att` sidecar cosign v3.1.1 wrote, and the DSSE envelope its one
    /// layer holds. Captured by `generate.py::_capture_attestation_sidecar`.
    const ATT_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/golden/attestation_sidecar_key_manifest.json");
    const ATT_ENVELOPE: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/golden/attestation_sidecar_key_envelope.json");
    const COSIGN_PUBLIC_KEY_PEM: &str = include_str!("../../../../../test/tests/fixtures/golden/keys/cosign.pub");
    const TRUSTED_ROOT_JSON: &[u8] = include_bytes!("../../../../../test/sigstore/trusted_root.json");

    /// cosign's **keyless** DSSE bundle, the source of the certificate and the
    /// envelope the keyless test repackages into an `.att` layer. cosign v3.1.1
    /// cannot write that layer itself (`ATT_CERTIFICATE_GAP`), so the material
    /// is cosign's even though the packaging is this test's — and the packaging
    /// is exactly the one `ATT_MANIFEST` pins.
    const KEYLESS_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/keyless_bundle.json");

    /// The rogue-CA leaf D authored for the simplesigning chain test, borrowed
    /// here for the same job: a certificate carrying the golden SAN and issuer
    /// that does **not** chain to the committed Fulcio root, so nothing
    /// downstream of the chain check can be what refuses it.
    const UNTRUSTED_CA_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/simplesigning/untrusted_ca_manifest.json");

    /// The local stack's Rekor **signer**, committed beside its public half and
    /// deliberately not a secret (`test/sigstore/README.md`).
    ///
    /// It is what lets a test mint a Signed Entry Timestamp over an entry of its
    /// own choosing. Without it the only SET-valid entry available is the golden
    /// bundle's own, and every negative below — a body about another envelope, an
    /// `integratedTime` outside the certificate's window — would red at the SET
    /// check instead of at the gate it is aimed at, proving nothing about either.
    const REKOR_PRIVATE_KEY_PEM: &str = include_str!("../../../../../test/sigstore/keys/rekor.key.pem");

    /// The subject manifest every golden fixture signs, byte-for-byte —
    /// `push_minimal_image` over the fixed payload `b"ocx-golden-subject"`.
    /// Reproduced rather than committed, exactly as `pipeline`'s copy is, and
    /// tied to the fixtures by `the_att_fixture_binds_the_golden_subject`.
    const GOLDEN_SUBJECT_MANIFEST: &str = concat!(
        r#"{"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json", "#,
        r#""config": {"mediaType": "application/vnd.oci.empty.v1+json", "#,
        r#""digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a", "size": 2}, "#,
        r#""layers": [{"mediaType": "application/octet-stream", "#,
        r#""digest": "sha256:ee88d8a4c22bbe871bcee1c56bcc02377e249363600edcaf096ad7a5a862149f", "size": 18}]}"#,
    );

    /// The SAN and Fulcio issuer of the golden keyless leaf, as the test stack
    /// minted it — the pair `identity.rs` and `pipeline` both pin.
    const GOLDEN_IDENTITY: &str = "ocx-test@example.com";
    const GOLDEN_ISSUER: &str = "http://dex:5556/dex";

    const CYCLONEDX: &str = "https://cyclonedx.org/bom";
    const COSIGN_SIGN: &str = "https://sigstore.dev/cosign/sign/v1";

    fn golden_subject() -> Digest {
        Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes())
    }

    fn layer_of(manifest: &str) -> Descriptor {
        let parsed: ImageManifest = serde_json::from_str(manifest).expect("manifest parses");
        parsed.layers.into_iter().next().expect("the sidecar carries one layer")
    }

    fn verifier() -> Verifier {
        Verifier::new(
            RekorConfiguration::default(),
            TrustRoot::load_trusted_root_json(TRUSTED_ROOT_JSON).expect("the committed trusted root loads"),
        )
        .expect("the trusted root builds a verifier")
    }

    fn key_policy(pem: &str) -> CompiledPolicy {
        CompiledPolicy {
            builder: None,
            backends: vec![PolicyBackend::Key(
                sigstore::crypto::CosignVerificationKey::try_from_pem(pem.as_bytes()).expect("an SPKI PEM"),
            )],
        }
    }

    fn keyless_policy(identity: &str, issuer: &str) -> CompiledPolicy {
        CompiledPolicy {
            builder: None,
            backends: vec![PolicyBackend::Keyless(CompiledKeyless {
                identity: IdentityRule::Exact(identity.to_owned()),
                issuer: issuer.to_owned(),
            })],
        }
    }

    /// The committed local trust root, which pins the local Rekor public key —
    /// so a `dev.sigstore.cosign/bundle` annotation's SET verifies here with no
    /// container and no network.
    fn trust_root() -> TrustRoot {
        TrustRoot::load_trusted_root_json(TRUSTED_ROOT_JSON).expect("the committed trusted root loads")
    }

    fn rekor_url() -> Url {
        Url::parse("http://127.0.0.1:3000").expect("rekor url")
    }

    /// The default gate: transparency evidence is required, and an unpinned
    /// Rekor key would have to be fetched (which `offline` forbids, so no test
    /// can silently reach the network). The committed root pins the local key,
    /// so the real path is exercised rather than skipped.
    fn gate<'a>(
        verifier: &'a Verifier,
        policies: &'a [CompiledPolicy],
        root: &'a TrustRoot,
        rekor_url: &'a Url,
    ) -> SidecarVerification<'a> {
        SidecarVerification {
            verifier,
            policies,
            trust_root: root,
            rekor_url,
            offline: true,
            allow_unlogged: false,
            rekor_keys: crate::oci::verify::pipeline::RekorKeyMemo::default(),
        }
    }

    /// The same gate with `--allow-unlogged-signature`, which lifts the evidence
    /// requirement and — the tests below are what hold this — nothing else.
    fn permissive_gate<'a>(
        verifier: &'a Verifier,
        policies: &'a [CompiledPolicy],
        root: &'a TrustRoot,
        rekor_url: &'a Url,
    ) -> SidecarVerification<'a> {
        SidecarVerification {
            allow_unlogged: true,
            ..gate(verifier, policies, root, rekor_url)
        }
    }

    /// The golden keyless bundle's own DSSE envelope, re-serialized as the layer
    /// body an `.att` sidecar carries.
    fn golden_envelope() -> Vec<u8> {
        let bundle: serde_json::Value = serde_json::from_str(KEYLESS_BUNDLE).expect("the golden bundle is JSON");
        serde_json::to_vec(
            bundle
                .get("dsseEnvelope")
                .expect("a keyless bundle carries a DSSE envelope"),
        )
        .expect("the envelope re-serializes")
    }

    /// The golden keyless bundle's own Fulcio leaf, as the annotation carries it.
    fn golden_certificate_pem() -> String {
        let bundle: serde_json::Value = serde_json::from_str(KEYLESS_BUNDLE).expect("the golden bundle is JSON");
        let der = BASE64
            .decode(
                bundle
                    .pointer("/verificationMaterial/certificate/rawBytes")
                    .and_then(serde_json::Value::as_str)
                    .expect("a keyless bundle carries a certificate"),
            )
            .expect("rawBytes is base64");
        pem::encode(&pem::Pem::new("CERTIFICATE", der))
    }

    /// The golden bundle's `tlogEntries[0]`: a **real** `dsse:0.0.1` entry about
    /// the very envelope above, with a real `integratedTime`, `logIndex`,
    /// `logID` and Signed Entry Timestamp the committed trust root's Rekor key
    /// verifies.
    ///
    /// Every field is read out of the fixture rather than transcribed — a
    /// transcribed `logID` is a second source of truth, and the SET is a
    /// signature over all four.
    fn golden_entry() -> (Vec<u8>, i64, i64, String, Vec<u8>) {
        let bundle: serde_json::Value = serde_json::from_str(KEYLESS_BUNDLE).expect("the golden bundle is JSON");
        let entry = &bundle["verificationMaterial"]["tlogEntries"][0];
        let number = |field: &str| -> i64 {
            entry[field]
                .as_str()
                .expect("the entry field is a JSON string")
                .parse()
                .expect("the entry field is an integer")
        };
        let body = BASE64
            .decode(entry["canonicalizedBody"].as_str().expect("a canonical body"))
            .expect("the body is base64");
        let log_id = BASE64
            .decode(entry["logId"]["keyId"].as_str().expect("the entry names a log"))
            .expect("the log id is base64");
        let set = BASE64
            .decode(
                entry["inclusionPromise"]["signedEntryTimestamp"]
                    .as_str()
                    .expect("the golden entry carries a SET"),
            )
            .expect("the SET is base64");
        (
            body,
            number("integratedTime"),
            number("logIndex"),
            hex::encode(log_id),
            set,
        )
    }

    /// A Signed Entry Timestamp over `{body, integratedTime, logIndex, logID}`,
    /// minted with the committed local Rekor signer.
    ///
    /// The construction is `tlog::verify_set`'s read in reverse — RFC 8785 over
    /// the same four fields, ECDSA P-256 / SHA-256 in ASN.1 DER — written as a
    /// JSON literal rather than reusing that module's private `SetPayload`, so a
    /// drift in the field names is caught here instead of agreeing with itself.
    fn mint_set(body: &[u8], integrated_time: i64, log_index: i64, log_id_hex: &str) -> Vec<u8> {
        use p256::ecdsa::signature::Signer as _;
        use p256::ecdsa::{DerSignature, SigningKey};

        let payload = serde_json::json!({
            "body": BASE64.encode(body),
            "integratedTime": integrated_time,
            "logIndex": log_index,
            "logID": log_id_hex,
        });
        let canonical = serde_json_canonicalizer::to_vec(&payload).expect("the SET payload canonicalizes");
        let key = SigningKey::from(
            p256::SecretKey::from_sec1_pem(REKOR_PRIVATE_KEY_PEM).expect("the committed Rekor signer is SEC1 PEM"),
        );
        let signature: DerSignature = key.sign(&canonical);
        signature.to_bytes().to_vec()
    }

    /// The `dev.sigstore.cosign/bundle` annotation, in the Go struct tags cosign
    /// spells it with — the read twin of `sign::simplesigning_write`'s writer.
    fn offline_bundle_annotation(
        body: &[u8],
        integrated_time: i64,
        log_index: i64,
        log_id_hex: &str,
        set: &[u8],
    ) -> String {
        serde_json::json!({
            "SignedEntryTimestamp": BASE64.encode(set),
            "Payload": {
                "body": BASE64.encode(body),
                "integratedTime": integrated_time,
                "logIndex": log_index,
                "logID": log_id_hex,
            }
        })
        .to_string()
    }

    /// The annotation for the golden entry, verbatim: cosign's own body, cosign's
    /// own SET, nothing minted.
    fn golden_bundle_annotation() -> String {
        let (body, integrated_time, log_index, log_id, set) = golden_entry();
        offline_bundle_annotation(&body, integrated_time, log_index, &log_id, &set)
    }

    /// The keyless `.att` layer cosign 2.x wrote and cosign 3.1.1 cannot: the
    /// golden bundle's own DSSE envelope as the layer body, its own Fulcio leaf
    /// as the [`ANNOTATION_COSIGN_CERTIFICATE`] annotation, and its own
    /// transparency-log entry as the [`ANNOTATION_COSIGN_BUNDLE`] one.
    ///
    /// Returns the manifest JSON and the envelope bytes, so the caller seeds the
    /// same two artifacts a captured fixture would give it.
    fn keyless_att_sidecar() -> (String, Vec<u8>) {
        keyless_att_sidecar_with(
            &golden_certificate_pem(),
            golden_envelope(),
            Some(golden_bundle_annotation()),
        )
    }

    /// The same layer, with each of its three annotations open to substitution:
    /// a rogue certificate, a tampered envelope, a spliced or absent log entry.
    fn keyless_att_sidecar_with(
        certificate_pem: &str,
        envelope: Vec<u8>,
        bundle_annotation: Option<String>,
    ) -> (String, Vec<u8>) {
        let mut annotations = serde_json::Map::new();
        annotations.insert(
            ANNOTATION_COSIGN_CERTIFICATE.to_owned(),
            serde_json::Value::String(certificate_pem.to_owned()),
        );
        if let Some(annotation) = bundle_annotation {
            annotations.insert(
                ANNOTATION_COSIGN_BUNDLE.to_owned(),
                serde_json::Value::String(annotation),
            );
        }

        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "size": 0,
                "digest": Algorithm::Sha256.hash(b"").to_string(),
            },
            "layers": [{
                "mediaType": DSSE_ENVELOPE_MEDIA_TYPE,
                "size": envelope.len(),
                "digest": Algorithm::Sha256.hash(&envelope).to_string(),
                "annotations": annotations,
            }],
        });
        (manifest.to_string(), envelope)
    }

    /// Seed the `sha256-<hex>.att` tag and its envelope blob, so a test drives
    /// the real discovery door rather than calling the layer verifier directly.
    fn seed_att_tag(subject: &Digest, manifest: &str, envelope: &[u8]) -> (StubTransportData, native::Reference) {
        let image: native::Reference = "registry.example.com/ocx/tool:1.0".parse().expect("stub reference");
        let layer = layer_of(manifest);
        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(layer.digest.clone(), envelope.to_vec());
            let tag_ref = sibling_tag_reference(
                &image,
                super::super::simplesigning_read::sidecar_tag(subject, SidecarKind::Attestation),
            );
            inner.manifests.insert(
                tag_ref.to_string(),
                (manifest.as_bytes().to_vec(), layer.digest.clone()),
            );
        }
        (data, image)
    }

    async fn read_tag_with(
        data: StubTransportData,
        image: &native::Reference,
        subject: &Digest,
        verify: &SidecarVerification<'_>,
        predicate_type: Option<&PredicateType>,
    ) -> Option<AttestationSidecarScan> {
        let transport = StubTransport::new(data);
        read_attestation_sidecar_tag(
            &transport,
            image,
            subject,
            GOLDEN_SUBJECT_MANIFEST.as_bytes(),
            verify,
            predicate_type,
            u64::MAX,
        )
        .await
        .expect("the sidecar read does not fault")
    }

    /// The tag door under the default gate: transparency evidence required.
    async fn read_tag(
        data: StubTransportData,
        image: &native::Reference,
        subject: &Digest,
        policies: &[CompiledPolicy],
        predicate_type: Option<&PredicateType>,
    ) -> Option<AttestationSidecarScan> {
        let verifier = verifier();
        let root = trust_root();
        let url = rekor_url();
        read_tag_with(
            data,
            image,
            subject,
            &gate(&verifier, policies, &root, &url),
            predicate_type,
        )
        .await
    }

    /// The same door under `--allow-unlogged-signature`.
    async fn read_tag_unlogged(
        data: StubTransportData,
        image: &native::Reference,
        subject: &Digest,
        policies: &[CompiledPolicy],
        predicate_type: Option<&PredicateType>,
    ) -> Option<AttestationSidecarScan> {
        let verifier = verifier();
        let root = trust_root();
        let url = rekor_url();
        read_tag_with(
            data,
            image,
            subject,
            &permissive_gate(&verifier, policies, &root, &url),
            predicate_type,
        )
        .await
    }

    /// The one refusal a scan recorded, or a panic naming what it recorded
    /// instead. Every negative below asserts on a *kind*, never on `is_err()`:
    /// a refusal for a parse or chain reason would satisfy a bare "an error
    /// happened" while proving the gate under test never ran.
    fn sole_refusal(scan: &AttestationSidecarScan) -> &VerifyErrorKind {
        assert!(scan.matches.is_empty(), "expected no match, got {:?}", scan.matches);
        match scan.refused.as_slice() {
            [only] => &only.reason,
            other => panic!("expected exactly one refusal, got {other:?}"),
        }
    }

    /// Ties the committed fixture to the reproduced subject bytes before
    /// anything is built on either. Without it every assertion below could pass
    /// against a Statement bound to some other manifest, because the harness
    /// supplies both halves.
    #[test]
    fn the_att_fixture_binds_the_golden_subject() {
        let envelope = DsseEnvelope::parse(ATT_ENVELOPE).expect("the committed envelope parses");
        let statement: serde_json::Value = serde_json::from_slice(&envelope.payload).expect("the payload is JSON");
        assert_eq!(
            format!(
                "sha256:{}",
                statement
                    .pointer("/subject/0/digest/sha256")
                    .and_then(serde_json::Value::as_str)
                    .expect("the Statement binds a subject")
            ),
            golden_subject().to_string(),
            "the reproduced subject manifest must hash to the digest the `.att` Statement names",
        );
        // The envelope is committed VERBATIM (`provenance.json`), so the
        // manifest's layer digest is over exactly these bytes. Without this,
        // a regeneration that reformatted the envelope keeps every test here
        // green — they seed the blob under the recomputed digest — while a real
        // registry serves the recorded digest and the read fails.
        assert_eq!(
            Algorithm::Sha256.hash(ATT_ENVELOPE).to_string(),
            layer_of(ATT_MANIFEST).digest,
            "the committed envelope must be the bytes the committed manifest's layer addresses",
        );
    }

    /// The measured evidence for outcome (B), asserted on the bytes rather than
    /// left in prose: cosign's `.att` manifest declares **no** `artifactType`
    /// and **no** `subject`, so no referrer listing can reach it and there is no
    /// attestation artifact type for `pipeline`'s client-side filter to match.
    /// A future cosign that grows either must fail here, not silently leave the
    /// tag door as the only one.
    #[test]
    fn a_cosign_att_sidecar_is_reachable_by_tag_and_by_nothing_else() {
        let manifest: serde_json::Value = serde_json::from_str(ATT_MANIFEST).expect("the fixture is JSON");
        assert!(
            manifest.get("artifactType").is_none(),
            "cosign grew an attestation artifactType; referrer discovery must then be revisited",
        );
        assert!(
            manifest.get("subject").is_none(),
            "cosign started writing a subject on `.att`; it would then be a referrer",
        );
        assert_eq!(layer_of(ATT_MANIFEST).media_type, DSSE_ENVELOPE_MEDIA_TYPE);
    }

    /// **§WP5, `.att` key mode.** cosign's own `sha256-<hex>.att` sidecar is
    /// discovered by tag and verified against a `PolicyBackend::Key`.
    ///
    /// The assertion is on the returned match, not on "no error": it pins the
    /// signed predicateType, the subject the Statement bound, the layer that
    /// carried it, and that both identity fields are absent because a key-mode
    /// envelope has no certificate to read them from.
    #[tokio::test]
    async fn a_cosign_att_sidecar_verifies_against_a_pinned_key() {
        let subject = golden_subject();
        let (data, image) = seed_att_tag(&subject, ATT_MANIFEST, ATT_ENVELOPE);
        let scan = read_tag(data, &image, &subject, &[key_policy(COSIGN_PUBLIC_KEY_PEM)], None)
            .await
            .expect("the `.att` tag exists");

        assert!(scan.refused.is_empty(), "nothing was refused: {:?}", scan.refused);
        assert_eq!(scan.matches.len(), 1);
        let found = &scan.matches[0];
        assert_eq!(found.attestation.predicate_type, CYCLONEDX);
        assert_eq!(found.attestation.subject_digest, subject);
        assert_eq!(found.verified.result.subject_digest, subject);
        assert_eq!(
            found.verified.result.referrer_digest.to_string(),
            layer_of(ATT_MANIFEST).digest
        );
        assert_eq!(found.verified.result.key_backend, KeyBackendKind::File);
        assert_eq!(found.verified.result.discovery_method, DiscoveryMethod::SidecarTag);
        assert_eq!(found.verified.result.certificate_identity, None);
        assert_eq!(found.verified.result.certificate_oidc_issuer, None);
        // No transparency evidence is credited on this path, so neither is
        // invented — the synthetic entry `sidecar_bundle` builds is never
        // reported.
        assert_eq!(found.verified.result.signed_at, None);
        assert_eq!(found.verified.result.rekor_log_index, None);
    }

    /// The DSSE signature is genuinely checked: the same sidecar under a policy
    /// naming a *different* key is refused.
    ///
    /// Paired with the test above on purpose — an always-Ok signature check
    /// passes the first, an always-Err one passes this, and only the pair shows
    /// the verdict tracks the key.
    ///
    /// `signature_invalid`, not `identity_mismatch`: a key *was* tried and did
    /// not verify, which is what [`identity::matching_key_policies`] reports and
    /// what a caller scripting 65 as "this artifact did not verify" can act on.
    /// The identity kind is reserved there for a policy set naming no key at all.
    #[tokio::test]
    async fn an_att_sidecar_is_refused_by_a_policy_naming_another_key() {
        use p256::ecdsa::SigningKey;
        use p256::elliptic_curve::rand_core::OsRng;
        use p256::pkcs8::EncodePublicKey as _;

        let other = SigningKey::random(&mut OsRng);
        let other_pem = other
            .verifying_key()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("a P-256 public key encodes as SPKI PEM");

        let subject = golden_subject();
        let (data, image) = seed_att_tag(&subject, ATT_MANIFEST, ATT_ENVELOPE);
        let scan = read_tag(data, &image, &subject, &[key_policy(&other_pem)], None)
            .await
            .expect("the `.att` tag exists");

        assert!(
            matches!(sole_refusal(&scan), VerifyErrorKind::SignatureInvalid),
            "another key must not verify cosign's envelope: {:?}",
            scan.refused,
        );
    }

    /// The cross-subject splice guard. A genuine attestation of *another*
    /// manifest, re-attached under this subject's `.att` tag, is valid in every
    /// other respect — the Statement's own subject binding is the only thing
    /// that refuses it.
    #[tokio::test]
    async fn an_att_sidecar_bound_to_another_manifest_is_refused() {
        let foreign = Algorithm::Sha256.hash(b"some other manifest entirely");
        let (data, image) = seed_att_tag(&foreign, ATT_MANIFEST, ATT_ENVELOPE);
        let scan = read_tag(data, &image, &foreign, &[key_policy(COSIGN_PUBLIC_KEY_PEM)], None)
            .await
            .expect("the `.att` tag exists");

        assert!(scan.matches.is_empty());
        assert!(
            matches!(
                scan.refused.as_slice(),
                [RefusedCandidate {
                    reason: VerifyErrorKind::StatementSubjectMismatch { .. },
                    ..
                }]
            ),
            "expected a subject-binding refusal, got {:?}",
            scan.refused
        );
    }

    /// **S-017 on the `.att` door.** A narrowing miss is neither a match nor a
    /// defect: asking for a predicateType this sidecar does not carry yields an
    /// empty scan with **nothing refused**, so a caller reports not-found
    /// rather than a malformed candidate.
    #[tokio::test]
    async fn an_att_sidecar_of_another_predicate_type_narrows_without_refusing() {
        let subject = golden_subject();
        let (data, image) = seed_att_tag(&subject, ATT_MANIFEST, ATT_ENVELOPE);
        let wanted = PredicateType::Uri(COSIGN_SIGN.to_owned());
        let scan = read_tag(
            data,
            &image,
            &subject,
            &[key_policy(COSIGN_PUBLIC_KEY_PEM)],
            Some(&wanted),
        )
        .await
        .expect("the `.att` tag exists");

        assert!(scan.matches.is_empty());
        assert!(
            scan.refused.is_empty(),
            "a narrowing miss records no refusal: {:?}",
            scan.refused
        );
    }

    /// An absent tag is "no legacy attestation", never an error — the
    /// overwhelmingly common case, and the one that must not cost a verdict.
    #[tokio::test]
    async fn an_absent_att_tag_is_not_an_error() {
        let subject = golden_subject();
        let image: native::Reference = "registry.example.com/ocx/tool:1.0".parse().expect("stub reference");
        let scan = read_tag(
            StubTransportData::new(),
            &image,
            &subject,
            &[key_policy(COSIGN_PUBLIC_KEY_PEM)],
            None,
        )
        .await;
        assert!(scan.is_none(), "a missing `.att` tag reads as absent, not as a fault");
    }

    /// **§WP5, `.att` keyless.** The shape cosign 2.x's `attest` wrote — a DSSE
    /// envelope layer, a `dev.sigstore.cosign/certificate` annotation and a
    /// `dev.sigstore.cosign/bundle` one — passes the full keyless gate: chain to
    /// the committed trust root, embedded SCT, the DSSE signature over the PAE,
    /// SAN + OIDC issuer, the logged body bound to this envelope, and the
    /// certificate's window against the entry's own `integratedTime`.
    ///
    /// The identity and issuer are read back off the certificate that just
    /// passed all of it, and the instant and the log position off the entry that
    /// just passed the SET check — so a pass proves the gate ran on real
    /// material rather than that a call returned `Ok`. Every reported value is
    /// read out of the fixture rather than transcribed.
    #[tokio::test]
    async fn a_keyless_att_sidecar_passes_the_full_keyless_gate() {
        let subject = golden_subject();
        let (manifest, envelope) = keyless_att_sidecar();
        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let scan = read_tag(
            data,
            &image,
            &subject,
            &[keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
            None,
        )
        .await
        .expect("the `.att` tag exists");

        assert!(scan.refused.is_empty(), "nothing was refused: {:?}", scan.refused);
        assert_eq!(scan.matches.len(), 1);
        let found = &scan.matches[0];
        assert_eq!(found.verified.result.key_backend, KeyBackendKind::Keyless);
        assert_eq!(
            found.verified.result.certificate_identity.as_deref(),
            Some(GOLDEN_IDENTITY)
        );
        assert_eq!(
            found.verified.result.certificate_oidc_issuer.as_deref(),
            Some(GOLDEN_ISSUER)
        );
        // The golden keyless capture is cosign's own image *signature*: an
        // in-toto Statement with the cosign/sign predicateType. Asserted so the
        // test is pinned to the material it actually repackaged.
        assert_eq!(found.attestation.predicate_type, COSIGN_SIGN);

        let (_, integrated_time, log_index, ..) = golden_entry();
        assert_eq!(
            found.verified.result.signed_at,
            Some(u64::try_from(integrated_time).expect("the fixture's instant is non-negative")),
            "the entry's own integratedTime is the only signing instant there is",
        );
        assert_eq!(
            found.verified.result.rekor_log_index,
            Some(u64::try_from(log_index).expect("the fixture's log index is non-negative")),
        );
    }

    /// **The evidence gate, and the pair that makes either half mean anything.**
    /// The same keyless layer with **no** `dev.sigstore.cosign/bundle`
    /// annotation is refused by default and verifies under
    /// `--allow-unlogged-signature`.
    ///
    /// Alone, the refusal is satisfied by a gate that refuses every keyless
    /// `.att`; alone, the acceptance is satisfied by one that refuses none.
    /// Asserted as `SignatureInvalid` rather than as "an error", because a
    /// refusal for a parse or chain reason would pass a bare `is_err()` while
    /// proving the gate never ran.
    ///
    /// Both absences are asserted on the accepting half too: nothing
    /// timestamped this envelope, so a flag that bought acceptance *and*
    /// invented a signing instant would be worse than the contract it replaced.
    #[tokio::test]
    async fn a_keyless_att_sidecar_with_no_transparency_evidence_is_refused_unless_opted_out() {
        let subject = golden_subject();
        let (manifest, envelope) = keyless_att_sidecar_with(&golden_certificate_pem(), golden_envelope(), None);
        let policies = [keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)];

        // The layer really is the no-evidence shape: assert it before asserting
        // anything about how it is judged.
        assert!(
            layer_of(&manifest)
                .annotations
                .as_ref()
                .is_none_or(|annotations| !annotations.contains_key(ANNOTATION_COSIGN_BUNDLE)),
            "this half of the pair must carry no offline Rekor bundle",
        );

        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let refused = read_tag(data, &image, &subject, &policies, None)
            .await
            .expect("the `.att` tag exists");
        assert!(
            matches!(sole_refusal(&refused), VerifyErrorKind::SignatureInvalid),
            "a keyless `.att` with no transparency-log evidence must be refused: {:?}",
            refused.refused,
        );

        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let accepted = read_tag_unlogged(data, &image, &subject, &policies, None)
            .await
            .expect("the `.att` tag exists");
        assert!(
            accepted.refused.is_empty(),
            "the opt-out must bring back the layer the gate refuses: {:?}",
            accepted.refused
        );
        assert_eq!(accepted.matches.len(), 1);
        let found = &accepted.matches[0];
        assert_eq!(found.verified.result.key_backend, KeyBackendKind::Keyless);
        assert_eq!(
            found.verified.result.certificate_identity.as_deref(),
            Some(GOLDEN_IDENTITY)
        );
        assert_eq!(
            found.verified.result.signed_at, None,
            "the opt-out accepts an envelope nothing timestamps; it must not report an instant",
        );
        assert_eq!(
            found.verified.result.rekor_log_index, None,
            "the opt-out accepts an envelope no log holds; it must not report a log position",
        );
    }

    /// **The logged body must be about *this* envelope.** A real SET over a real
    /// entry for another artifact proves nothing about the bytes in hand, so the
    /// `dsse:0.0.1` body is bound to the envelope's payload and its signature.
    ///
    /// Both spliced bodies are minted with the committed Rekor signer, so their
    /// SETs are genuine and the SET check passes — which is what makes
    /// `TlogBindingMismatch`, rather than `RekorSetInvalid`, the proof that the
    /// binding is what refused them. The golden entry beside them (the test
    /// above) is the accepting half.
    #[tokio::test]
    async fn a_logged_body_about_another_envelope_is_refused() {
        let subject = golden_subject();
        let policies = [keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)];
        let (body, integrated_time, log_index, log_id, _) = golden_entry();
        let genuine: serde_json::Value = serde_json::from_slice(&body).expect("the golden body is JSON");

        // A body about another artifact: the same entry with a `payloadHash`
        // that is a real SHA-256 of other bytes rather than a value chosen to
        // differ.
        let mut other_payload = genuine.clone();
        other_payload["spec"]["payloadHash"]["value"] = serde_json::Value::String(
            Algorithm::Sha256
                .hash(b"some other statement entirely")
                .hex()
                .to_owned(),
        );
        // The same entry re-pointed at another signature over the same bytes.
        let mut other_signature = genuine.clone();
        other_signature["spec"]["signatures"][0]["signature"] =
            serde_json::Value::String(BASE64.encode(b"not the signature this envelope carries"));

        for (label, spliced) in [("payloadHash", other_payload), ("signature", other_signature)] {
            let spliced = serde_json::to_vec(&spliced).expect("the spliced body serializes");
            assert_ne!(spliced, body, "the {label} mutation must land, or this asserts nothing");
            let annotation = offline_bundle_annotation(
                &spliced,
                integrated_time,
                log_index,
                &log_id,
                &mint_set(&spliced, integrated_time, log_index, &log_id),
            );
            let (manifest, envelope) =
                keyless_att_sidecar_with(&golden_certificate_pem(), golden_envelope(), Some(annotation));
            let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
            let scan = read_tag(data, &image, &subject, &policies, None)
                .await
                .expect("the `.att` tag exists");
            assert!(
                matches!(sole_refusal(&scan), VerifyErrorKind::TlogBindingMismatch),
                "a logged body with a spliced {label} must not bind: {:?}",
                scan.refused,
            );
        }
    }

    /// **The anchor is the log's instant, not the certificate's own claim.**
    ///
    /// A genuine, SET-valid entry whose `integratedTime` falls an hour past the
    /// leaf's ten-minute window is refused. This is the test the old contract
    /// could not have failed: `sidecar_bundle` hardcoded the certificate's own
    /// `notBefore` as the entry's `integrated_time` and `verify_keyless` passed
    /// the same value as the signing instant, so both the library's expiry check
    /// and OCX's own asked the certificate when it was valid and then judged the
    /// certificate against that answer. Every `integratedTime` verified,
    /// including this one.
    ///
    /// The refusal arrives as `cert_chain_invalid` because `sigstore` reaches it
    /// first — it builds the chain *at* the anchor, and a leaf that was not yet
    /// valid then does not chain. That it can now fail at all is the point; OCX's
    /// own re-assertion of row 13 sits behind it as the redundancy CVE-2024-55655
    /// exists for.
    #[tokio::test]
    async fn an_entry_outside_the_certificate_window_is_refused() {
        let subject = golden_subject();
        let (body, integrated_time, log_index, log_id, _) = golden_entry();
        // Read the window off the fixture's own certificate rather than
        // transcribing it, then step an hour past `notAfter`.
        let leaf = layer_certificate(&layer_of(&keyless_att_sidecar().0))
            .expect("the layer carries a certificate")
            .expect("the layer carries a certificate");
        let cert = parse_certificate(&leaf).expect("the golden leaf parses");
        let not_after =
            i64::try_from(cert.tbs_certificate.validity.not_after.to_unix_duration().as_secs()).expect("a real window");
        let outside = not_after + 3600;
        assert!(
            integrated_time <= not_after,
            "the genuine entry must be inside the window, or the pair proves nothing",
        );

        let annotation = offline_bundle_annotation(
            &body,
            outside,
            log_index,
            &log_id,
            &mint_set(&body, outside, log_index, &log_id),
        );
        let (manifest, envelope) =
            keyless_att_sidecar_with(&golden_certificate_pem(), golden_envelope(), Some(annotation));
        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let scan = read_tag(
            data,
            &image,
            &subject,
            &[keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
            None,
        )
        .await
        .expect("the `.att` tag exists");
        assert!(
            matches!(sole_refusal(&scan), VerifyErrorKind::CertChainInvalid),
            "an entry outside the leaf's window must be refused: {:?}",
            scan.refused,
        );
    }

    /// **The chain gate.** A keyless `.att` whose certificate does not chain to
    /// the committed Fulcio root is refused with `cert_chain_invalid`.
    ///
    /// The rogue leaf carries the *same* SAN and OIDC issuer as the genuine one,
    /// so nothing downstream of the chain check can be what refuses it — the
    /// identity gate would accept this certificate. Run under the opt-out, so
    /// the missing-entry refusal (which is also a refusal, and would be reached
    /// on this layer) cannot be what the verdict reports.
    #[tokio::test]
    async fn a_keyless_att_certificate_outside_the_fulcio_root_is_refused() {
        let rogue = layer_of(UNTRUSTED_CA_MANIFEST)
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(ANNOTATION_COSIGN_CERTIFICATE).cloned())
            .expect("the fixture carries a certificate");
        let cert = parse_certificate(&pem::parse(&rogue).expect("the rogue annotation is PEM").into_contents())
            .expect("the rogue certificate parses");
        assert_eq!(
            subject_identity(&cert).as_deref(),
            Some(GOLDEN_IDENTITY),
            "the rogue certificate must be identity-acceptable, or the chain is not what refuses it",
        );
        assert_eq!(oidc_issuer(&cert).as_deref(), Some(GOLDEN_ISSUER));

        let subject = golden_subject();
        let (manifest, envelope) = keyless_att_sidecar_with(&rogue, golden_envelope(), None);
        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let scan = read_tag_unlogged(
            data,
            &image,
            &subject,
            &[keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
            None,
        )
        .await
        .expect("the `.att` tag exists");
        assert!(
            matches!(sole_refusal(&scan), VerifyErrorKind::CertChainInvalid),
            "a certificate outside the trust root must be refused: {:?}",
            scan.refused,
        );
    }

    /// **The delegated signature check.** The genuine certificate over an
    /// envelope with one flipped signature byte is refused.
    ///
    /// Separate from the chain test on purpose — chain, SCT and signature are
    /// one delegated `sigstore` call, so only two inputs that break different
    /// halves of it can tell the halves apart. Run under the opt-out for the
    /// same reason as the chain test: `SignatureInvalid` is also the
    /// missing-entry refusal's kind, so the flag is what makes this verdict
    /// attributable to the signature.
    #[tokio::test]
    async fn a_tampered_att_envelope_signature_is_refused() {
        let genuine = golden_envelope();
        let mut parsed: serde_json::Value = serde_json::from_slice(&genuine).expect("the envelope is JSON");
        let signature = parsed["signatures"][0]["sig"].as_str().expect("a base64 signature");
        let mut raw = BASE64.decode(signature).expect("the signature is base64");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        parsed["signatures"][0]["sig"] = serde_json::Value::String(BASE64.encode(&raw));
        let tampered = serde_json::to_vec(&parsed).expect("the envelope re-serializes");
        assert_ne!(tampered, genuine, "the mutation must land, or this asserts nothing");

        let subject = golden_subject();
        let (manifest, envelope) = keyless_att_sidecar_with(&golden_certificate_pem(), tampered, None);
        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let scan = read_tag_unlogged(
            data,
            &image,
            &subject,
            &[keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
            None,
        )
        .await
        .expect("the `.att` tag exists");
        assert!(
            matches!(sole_refusal(&scan), VerifyErrorKind::SignatureInvalid),
            "a tampered envelope signature must be refused: {:?}",
            scan.refused,
        );
    }

    /// **#103 on the keyless arm.** A `builder`-pinned trust policy is enforced
    /// for a keyless `.att`, not only for the key-mode one beside it.
    ///
    /// Driven at [`verify_keyless`] rather than through the tag door, because no
    /// committed fixture carries keyless-signed SLSA provenance: the golden
    /// keyless envelope is cosign's image-signature Statement, which the pin is
    /// deliberately inert on, and a Fulcio leaf cannot be re-signed over other
    /// bytes. What the seam proves is the wiring — that the *matched* policy set
    /// reaches [`dsse::enforce_builder_pin`] at all, which is what discarding
    /// [`matching_policies`]'s return value silently cost. The pin's own refusal
    /// logic is `dsse`'s and is tested there.
    ///
    /// Both directions, over the same real certificate, bundle and subject: the
    /// pin the provenance names verifies, another pin refuses.
    #[tokio::test]
    async fn a_builder_pinned_policy_is_enforced_on_the_keyless_att_arm() {
        const BUILDER: &str = "https://github.com/ocx-sh/ocx/.github/workflows/release.yml@refs/heads/main";

        // A provenance attestation, standing in for the one no fixture carries.
        // Only `enforce_builder_pin` reads it; every cryptographic input below
        // is the genuine golden material.
        let provenance = |builder: &str| dsse::VerifiedAttestation {
            predicate_type: "https://slsa.dev/provenance/v1".to_owned(),
            payload: Vec::new(),
            predicate: serde_json::value::RawValue::from_string(
                serde_json::json!({ "runDetails": { "builder": { "id": builder } } }).to_string(),
            )
            .expect("the predicate is JSON"),
            subject_digest: golden_subject(),
        };

        // No bundle annotation, and the opt-out below is what makes that legal:
        // the fabricated attestation carries no payload for the logged body to
        // bind to, and the pin is what this test is aimed at.
        let layer = layer_of(&keyless_att_sidecar_with(&golden_certificate_pem(), golden_envelope(), None).0);
        let envelope = DsseEnvelope::parse(&golden_envelope()).expect("the golden envelope parses");
        let signature = envelope.signatures[0].sig.clone();
        let verifier = verifier();
        let root = trust_root();
        let url = rekor_url();

        for (pin, expected_pass) in [(BUILDER, true), ("https://elsewhere.example/build", false)] {
            let policies = [CompiledPolicy {
                builder: Some(pin.to_owned()),
                backends: vec![PolicyBackend::Keyless(CompiledKeyless {
                    identity: IdentityRule::Exact(GOLDEN_IDENTITY.to_owned()),
                    issuer: GOLDEN_ISSUER.to_owned(),
                })],
            }];
            let verify = permissive_gate(&verifier, &policies, &root, &url);
            let keyless = gather_keyless(
                &layer,
                layer_certificate(&layer)
                    .expect("the layer carries a certificate")
                    .expect("the layer carries a certificate"),
                &verify,
            )
            .await
            .expect("the golden keyless material reads");
            let bundle = sidecar_bundle(&envelope, Some(&keyless), &signature).expect("the bundle builds");
            let verified = VerifiedEnvelope {
                attestation: provenance(BUILDER),
                signatures: envelope.signatures.clone(),
            };

            let verdict = verify_keyless(keyless, GOLDEN_SUBJECT_MANIFEST.as_bytes(), bundle, &verify, &verified).await;
            if expected_pass {
                // The signer read back off the certificate that just passed the
                // whole gate, not a bare `is_ok()`: a permissive `verify_keyless`
                // that skipped chain, SCT and identity satisfies "no error" while
                // proving nothing about what verified.
                let signer = verdict.expect("the pin the provenance names must verify");
                assert_eq!(signer.key_backend, KeyBackendKind::Keyless);
                assert_eq!(signer.certificate_identity.as_deref(), Some(GOLDEN_IDENTITY));
                assert_eq!(signer.certificate_oidc_issuer.as_deref(), Some(GOLDEN_ISSUER));
            } else {
                assert!(
                    matches!(verdict, Err(VerifyErrorKind::BuilderMismatch { .. })),
                    "a builder-pinned policy must refuse provenance from another builder: {verdict:?}",
                );
            }
        }
    }

    /// The keyless identity gate is not decorative: the same certificate under
    /// a policy naming another SAN is refused.
    #[tokio::test]
    async fn a_keyless_att_sidecar_is_refused_under_another_identity() {
        let subject = golden_subject();
        let (manifest, envelope) = keyless_att_sidecar();
        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let scan = read_tag(
            data,
            &image,
            &subject,
            &[keyless_policy("someone-else@example.com", GOLDEN_ISSUER)],
            None,
        )
        .await
        .expect("the `.att` tag exists");

        assert!(scan.matches.is_empty());
        assert!(
            matches!(
                scan.refused.as_slice(),
                [RefusedCandidate {
                    reason: VerifyErrorKind::IdentityMismatch,
                    ..
                }]
            ),
            "expected an identity refusal, got {:?}",
            scan.refused
        );
    }

    /// **Byte accounting.** A descriptor refused *before* any I/O costs nothing.
    ///
    /// Both layers here are rejected from the descriptor alone — one declares a
    /// size over the 32 MiB envelope cap, the other an unparseable digest — so no
    /// connection is opened and no blob is served (neither is seeded, which is
    /// the second statement that nothing was fetched). Charging the flat cap for
    /// them, as this reader did, spends 64 MiB of a run's cross-candidate budget
    /// on two zero-cost descriptors: a hostile registry could truncate the scan
    /// for free.
    ///
    /// The assertion is the exact manifest length, not "small": a bound loose
    /// enough to pass under the old code would prove nothing.
    #[tokio::test]
    async fn a_descriptor_refused_before_any_fetch_costs_no_bytes() {
        let subject = golden_subject();
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "size": 0,
                "digest": Algorithm::Sha256.hash(b"").to_string(),
            },
            "layers": [
                {
                    "mediaType": DSSE_ENVELOPE_MEDIA_TYPE,
                    "size": MAX_ATTESTATION_ENVELOPE_BYTES as u64 + 1,
                    "digest": Algorithm::Sha256.hash(b"over-cap").to_string(),
                },
                {
                    "mediaType": DSSE_ENVELOPE_MEDIA_TYPE,
                    "size": 1,
                    "digest": "not-a-digest",
                },
            ],
        })
        .to_string();

        let image: native::Reference = "registry.example.com/ocx/tool:1.0".parse().expect("stub reference");
        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            let tag_ref = sibling_tag_reference(
                &image,
                super::super::simplesigning_read::sidecar_tag(&subject, SidecarKind::Attestation),
            );
            inner
                .manifests
                .insert(tag_ref.to_string(), (manifest.as_bytes().to_vec(), String::new()));
        }
        let calls = data.clone();
        let scan = read_tag(data, &image, &subject, &[key_policy(COSIGN_PUBLIC_KEY_PEM)], None)
            .await
            .expect("the `.att` tag exists");

        // The kinds, not the count: both are `bundle_parse_failed`, and asserting
        // that is what shows the descriptor gate refused them rather than some
        // later stage the loop happened to reach.
        assert!(
            matches!(
                scan.refused.as_slice(),
                [
                    RefusedCandidate {
                        reason: VerifyErrorKind::BundleParseFailed,
                        ..
                    },
                    RefusedCandidate {
                        reason: VerifyErrorKind::BundleParseFailed,
                        ..
                    }
                ]
            ),
            "both descriptors are refused from the descriptor alone: {:?}",
            scan.refused,
        );
        assert_eq!(
            scan.bytes_read,
            manifest.len() as u64,
            "a descriptor rejected before the fetch must cost only the manifest that listed it",
        );
        // `bytes_read` alone cannot tell "never fetched" from "fetched an
        // unseeded blob and got zero bytes" — the stub serves a missing blob as
        // empty. The transport's own call log can: it records every
        // `pull_blob_streaming`, so an empty tally is the second, independent
        // statement that no connection was opened.
        assert!(
            !calls
                .read()
                .calls
                .iter()
                .any(|call| call.starts_with("pull_blob_streaming:")),
            "no blob may be fetched for a descriptor refused before the fetch: {:?}",
            calls.read().calls,
        );
    }

    /// **#103 on the key arm.** A `builder`-pinned trust policy is enforced for
    /// a key-mode `.att` too — the arm cosign v3.1.1 actually writes, and the
    /// one that shipped with the identical omission the keyless arm was fixed
    /// for.
    ///
    /// Driven through the real tag door, over a self-signed layer: no committed
    /// fixture carries key-signed SLSA provenance, and unlike the keyless arm
    /// there is no seam below the door to call — the pin lives inline in
    /// `verify_layer`. So the test mints a P-256 key, signs the PAE of a real
    /// provenance Statement with it, and pins that key in the policy; every
    /// check the arm runs is therefore genuine, and only the builder pin varies.
    ///
    /// Both directions over one layer: the pin the provenance names verifies,
    /// another pin refuses. Deleting `enforce_builder_pin` from the key arm
    /// reds the second half.
    #[tokio::test]
    async fn a_builder_pinned_policy_is_enforced_on_the_key_att_arm() {
        use p256::ecdsa::signature::Signer as _;
        use p256::ecdsa::{DerSignature, SigningKey};
        use p256::elliptic_curve::rand_core::OsRng;
        use p256::pkcs8::EncodePublicKey as _;

        const BUILDER: &str = "https://github.com/ocx-sh/ocx/.github/workflows/release.yml@refs/heads/main";
        const PROVENANCE: &str = "https://slsa.dev/provenance/v1";

        let subject = golden_subject();
        let statement = serde_json::json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{ "name": "pkg", "digest": { "sha256": subject.hex() } }],
            "predicateType": PROVENANCE,
            "predicate": { "runDetails": { "builder": { "id": BUILDER } } },
        })
        .to_string()
        .into_bytes();

        let key = SigningKey::random(&mut OsRng);
        let public_pem = key
            .verifying_key()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("a P-256 public key encodes as SPKI PEM");
        let signature: DerSignature = key.sign(&pae(DSSE_PAYLOAD_TYPE, &statement));
        let envelope = serde_json::to_vec(&DsseEnvelope {
            payload: statement,
            payload_type: DSSE_PAYLOAD_TYPE.to_owned(),
            signatures: vec![crate::oci::attest::dsse::DsseSignature {
                sig: signature.to_bytes().to_vec(),
                keyid: String::new(),
            }],
        })
        .expect("the envelope serializes");

        // A key-mode `.att` layer: the envelope carries its own signature and
        // there is no certificate annotation, so `verify_layer` takes the key arm.
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "size": 0,
                "digest": Algorithm::Sha256.hash(b"").to_string(),
            },
            "layers": [{
                "mediaType": DSSE_ENVELOPE_MEDIA_TYPE,
                "size": envelope.len(),
                "digest": Algorithm::Sha256.hash(&envelope).to_string(),
            }],
        })
        .to_string();

        for (pin, expected_pass) in [(BUILDER, true), ("https://elsewhere.example/build", false)] {
            let policies = [CompiledPolicy {
                builder: Some(pin.to_owned()),
                ..key_policy(&public_pem)
            }];
            let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
            let scan = read_tag(data, &image, &subject, &policies, None)
                .await
                .expect("the `.att` tag exists");

            if expected_pass {
                assert!(scan.refused.is_empty(), "nothing was refused: {:?}", scan.refused);
                assert_eq!(scan.matches.len(), 1);
                assert_eq!(scan.matches[0].attestation.predicate_type, PROVENANCE);
                assert_eq!(scan.matches[0].verified.result.key_backend, KeyBackendKind::File);
            } else {
                assert!(
                    matches!(sole_refusal(&scan), VerifyErrorKind::BuilderMismatch { .. }),
                    "a builder-pinned policy must refuse provenance from another builder: {:?}",
                    scan.refused,
                );
            }
        }
    }

    /// **The structural check runs before the transparency-log work.**
    ///
    /// A layer whose `dev.sigstore.cosign/bundle` annotation is unreadable, but
    /// whose signed predicateType is not the one asked for, must still be an
    /// `Ok(None)` narrowing miss — **not** a refusal. Reading the annotation
    /// first is what lets a registry turn `attestation_not_found` into
    /// `bundle_parse_failed` by corrupting the annotation of a document nobody
    /// asked for (S-017), and it spends a Rekor key resolution on a document
    /// already known to be the wrong type.
    ///
    /// The pair is the same corrupt layer under **no** narrowing: there the
    /// annotation genuinely is read, and genuinely does refuse. Either half
    /// alone is satisfied by a reader that never reads the annotation at all.
    #[tokio::test]
    async fn a_narrowing_miss_outruns_a_corrupt_transparency_annotation() {
        let subject = golden_subject();
        let policies = [keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)];
        let (manifest, envelope) = keyless_att_sidecar_with(
            &golden_certificate_pem(),
            golden_envelope(),
            Some("{not a bundle annotation".to_owned()),
        );

        // The golden keyless envelope carries cosign's image-signature
        // predicateType, so asking for CycloneDX narrows it away.
        let wanted = PredicateType::Uri(CYCLONEDX.to_owned());
        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let narrowed = read_tag(data, &image, &subject, &policies, Some(&wanted))
            .await
            .expect("the `.att` tag exists");
        assert!(narrowed.matches.is_empty());
        assert!(
            narrowed.refused.is_empty(),
            "a document of another predicateType must narrow away before its annotation is read: {:?}",
            narrowed.refused,
        );

        let (data, image) = seed_att_tag(&subject, &manifest, &envelope);
        let unnarrowed = read_tag(data, &image, &subject, &policies, None)
            .await
            .expect("the `.att` tag exists");
        assert!(
            matches!(sole_refusal(&unnarrowed), VerifyErrorKind::BundleParseFailed),
            "the same corrupt annotation must still refuse when the document is the one asked for: {:?}",
            unnarrowed.refused,
        );
    }

    /// **A truncated walk says so.** Both bounds that can stop the layer loop
    /// are reported on the scan, so the caller can fail closed — a
    /// `tracing::debug!` is invisible to it, and collect-all is the mode that
    /// cannot answer "every attestation" from a prefix.
    ///
    /// Each bound is paired with the run that does **not** hit it, so neither
    /// assertion is satisfied by a reader that reports truncation always or
    /// never.
    #[tokio::test]
    async fn a_truncated_att_walk_reports_the_bound_that_stopped_it() {
        let subject = golden_subject();
        let policies = [key_policy(COSIGN_PUBLIC_KEY_PEM)];

        // The candidate cap, over a manifest repeating the one golden layer.
        let repeated = |count: usize| {
            let mut parsed: serde_json::Value = serde_json::from_str(ATT_MANIFEST).expect("the fixture is JSON");
            let layer = parsed["layers"][0].clone();
            parsed["layers"] = serde_json::Value::Array(vec![layer; count]);
            parsed.to_string()
        };

        for (count, expected) in [
            (MAX_ATTESTATION_CANDIDATES, None),
            (MAX_ATTESTATION_CANDIDATES + 1, Some(ScanStop::CandidateCap)),
        ] {
            let manifest = repeated(count);
            let (data, image) = seed_att_tag(&subject, &manifest, ATT_ENVELOPE);
            let scan = read_tag(data, &image, &subject, &policies, None)
                .await
                .expect("the `.att` tag exists");
            assert_eq!(
                scan.stop, expected,
                "{count} DSSE layers against a cap of {MAX_ATTESTATION_CANDIDATES}"
            );
            assert_eq!(
                scan.matches.len(),
                count.min(MAX_ATTESTATION_CANDIDATES),
                "the reader returns every layer it examined; dedup is the caller's",
            );
        }

        // The byte budget, over the ordinary one-layer sidecar: a budget that
        // the manifest read alone exhausts stops the walk before its first blob.
        for (budget, expected) in [
            (u64::MAX, None),
            (ATT_MANIFEST.len() as u64, Some(ScanStop::ByteBudget)),
        ] {
            let (data, image) = seed_att_tag(&subject, ATT_MANIFEST, ATT_ENVELOPE);
            let transport = StubTransport::new(data);
            let verifier = verifier();
            let root = trust_root();
            let url = rekor_url();
            let scan = read_attestation_sidecar_tag(
                &transport,
                &image,
                &subject,
                GOLDEN_SUBJECT_MANIFEST.as_bytes(),
                &gate(&verifier, &policies, &root, &url),
                None,
                budget,
            )
            .await
            .expect("the sidecar read does not fault")
            .expect("the `.att` tag exists");
            assert_eq!(scan.stop, expected, "byte budget {budget}");
            assert_eq!(scan.matches.len(), usize::from(expected.is_none()));
        }
    }

    /// **Row 13's own red, on the `.att` leaf.**
    ///
    /// `an_entry_outside_the_certificate_window_is_refused` above proves the
    /// *anchor* is the log's instant, but the refusal it observes comes from
    /// `sigstore`'s chain build, which reaches the out-of-window entry first —
    /// so OCX's own re-assertion of CVE-2024-55655 has no reachable red through
    /// that door. This calls it directly, on the very certificate the keyless
    /// `.att` arm judges, so the two guards are proven separately.
    #[test]
    fn the_att_leaf_window_guard_refuses_an_instant_outside_it() {
        let leaf = layer_certificate(&layer_of(&keyless_att_sidecar().0))
            .expect("the layer carries a certificate")
            .expect("the layer carries a certificate");
        let cert = parse_certificate(&leaf).expect("the golden leaf parses");
        let not_before = i64::try_from(cert.tbs_certificate.validity.not_before.to_unix_duration().as_secs())
            .expect("a real window");
        let not_after =
            i64::try_from(cert.tbs_certificate.validity.not_after.to_unix_duration().as_secs()).expect("a real window");

        assert!(
            tlog::verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(not_before), &cert).is_ok(),
            "the window is inclusive at its lower end",
        );
        for outside in [not_before - 1, not_after + 1] {
            assert!(
                matches!(
                    tlog::verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(outside), &cert),
                    Err(VerifyErrorKind::CertificateValidityWindow { .. })
                ),
                "an instant at {outside} is outside [{not_before}, {not_after}] and must be refused",
            );
        }
    }

    /// A layer whose media type is not the DSSE envelope one is **skipped**,
    /// not refused — a sidecar legitimately carries other layers, and the same
    /// tolerance its simplesigning sibling applies.
    #[tokio::test]
    async fn a_non_dsse_layer_is_skipped_rather_than_refused() {
        let subject = golden_subject();
        let manifest = ATT_MANIFEST.replace(DSSE_ENVELOPE_MEDIA_TYPE, "application/octet-stream");
        assert!(
            !manifest.contains(DSSE_ENVELOPE_MEDIA_TYPE),
            "the mutation must land, or this asserts nothing",
        );
        let (data, image) = seed_att_tag(&subject, &manifest, ATT_ENVELOPE);
        let scan = read_tag(data, &image, &subject, &[key_policy(COSIGN_PUBLIC_KEY_PEM)], None)
            .await
            .expect("the `.att` tag exists");

        assert!(scan.matches.is_empty());
        assert!(
            scan.refused.is_empty(),
            "a foreign layer is skipped: {:?}",
            scan.refused
        );
    }
}
