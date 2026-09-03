// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Specification tests for the mirror-facing discovery seam.
//!
//! **OCX-C-2** — [`list_signature_candidates`] lists *signature* candidates
//! and nothing else: referrers-with-fallback filtered to signature-typed
//! artifacts, plus the `sha256-<hex>.sig` sidecar tag. `.att` and `.sbom` are
//! excluded by the 2026-09-02 ruling, because counting them would let the
//! mirror's backfill skip a subject that carries no signature at all.
//!
//! **OCX-C-1** — the transport factory's signature, and the clause that says
//! `oci/copy.rs` stays untouched by this seam.
//!
//! Every candidate this module lists is **unverified**: no certificate chain
//! is checked, no Rekor entry fetched, no policy consulted. The identity tests
//! below assert that the fields are *populated*, never that they are true —
//! that distinction is the type's entire safety story.
//!
//! Fixtures are the committed G0 captures the `verify` pipeline already uses,
//! reached with `include_str!` so a moved fixture is a compile error rather
//! than a runtime skip. The transport double is `StubTransport`, likewise
//! shared — a parallel double would drift from the one the rest of the crate
//! is specified against.

use super::*;

use crate::oci::client::sibling_tag_reference;
use crate::oci::client::test_transport::{StubTransport, StubTransportData};
use crate::oci::referrer::ReferrerManifest;
use crate::oci::referrer::media_types::{COSIGN_SBOM_ARTIFACT_TYPE, SIGSTORE_BUNDLE_V03};
use crate::oci::verify::simplesigning_read::{SidecarKind, sidecar_tag};
use crate::oci::{Algorithm, Descriptor, Digest, OCI_IMAGE_MEDIA_TYPE};

// ── fixtures ───────────────────────────────────────────────────────────

/// cosign v3.1.1's keyless signature bundle, captured in G0. Its
/// `verificationMaterial` is a Fulcio certificate and no public key, which is
/// the half OCX-C-2 reads `certificate_identity` / `certificate_issuer` off.
const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../../../test/tests/fixtures/golden/keyless_bundle.json");

/// cosign's **key-mode** bundle from the same capture: a `publicKey` + `hint`
/// with no certificate anywhere. The mirror image of the constant above, and
/// the reason the two identity tests cannot both pass against a stub that
/// hardcodes one shape.
const GOLDEN_KEY_BUNDLE: &str = include_str!("../../../../../../test/tests/fixtures/golden/key_bundle.json");

/// A real cosign `sha256-<hex>.sig` sidecar manifest, so the sidecar door is
/// exercised against bytes cosign wrote rather than against a hand-built
/// manifest that could agree with a wrong reader.
const GOLDEN_SIMPLESIGNING_MANIFEST: &str =
    include_str!("../../../../../../test/tests/fixtures/golden/simplesigning_key_manifest.json");

/// The SAN and Fulcio issuer of the golden keyless leaf, as the local test
/// stack minted them. Transcribed rather than read from a JSON field because
/// they live inside the DER certificate; `pipeline.rs` pins the same two
/// values against the same fixture.
const GOLDEN_IDENTITY: &str = "ocx-test@example.com";
const GOLDEN_ISSUER: &str = "http://dex:5556/dex";

/// The descriptor ceiling `oci/client/transport.rs` enforces on a fallback
/// referrers index (`MAX_FALLBACK_DESCRIPTORS`, private to that module and so
/// not nameable here).
///
/// Restated rather than imported, and the pair of assertions in
/// [`the_fallback_descriptor_ceiling_is_the_transports_and_is_not_bypassed`]
/// is what keeps the restatement honest: one index at the ceiling that must be
/// read, one past it that must be refused. A copy that drifted from the real
/// constant reds one of the two.
const TRANSPORT_FALLBACK_DESCRIPTOR_CEILING: usize = 4096;

// ── helpers ────────────────────────────────────────────────────────────

fn image() -> oci::native::Reference {
    "registry.example/team/demo:1.0".parse().expect("stub reference")
}

/// The subject every candidate below is attached to.
fn subject() -> Digest {
    Digest::Sha256("a".repeat(64))
}

/// A referrer manifest carrying `bundle` as its single payload layer, typed
/// `artifact_type`.
///
/// Returns the manifest bytes; the caller pushes them through the stub's own
/// `push_referrer_manifest` so the referrers index and the manifest store are
/// populated exactly the way a registry populates them.
fn referrer_manifest(subject_digest: &Digest, artifact_type: &str, bundle: &[u8]) -> Vec<u8> {
    let payload = Descriptor {
        media_type: SIGSTORE_BUNDLE_V03.to_string(),
        digest: Algorithm::Sha256.hash(bundle).to_string(),
        size: bundle.len() as i64,
        ..Descriptor::default()
    };
    let subject_descriptor = Descriptor {
        media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
        digest: subject_digest.to_string(),
        size: 7,
        ..Descriptor::default()
    };
    ReferrerManifest::build(subject_descriptor, artifact_type, payload, None)
        .to_canonical_json()
        .expect("referrer manifest serializes")
}

/// Seeds one referrer of `artifact_type` over `bundle`, through the stub's own
/// push path, and returns its manifest digest.
async fn seed_referrer(data: &StubTransportData, artifact_type: &str, bundle: &[u8]) -> String {
    data.write().capture_pushes = true;
    data.write()
        .blobs
        .insert(Algorithm::Sha256.hash(bundle).to_string(), bundle.to_vec());
    let transport = StubTransport::new(data.clone());
    let bytes = referrer_manifest(&subject(), artifact_type, bundle);
    transport
        .push_referrer_manifest(&image(), &subject(), &bytes, OCI_IMAGE_MEDIA_TYPE)
        .await
        .expect("the stub accepts a referrer push")
        .digest
}

/// The golden keyless bundle with its Statement's `predicateType` swapped for
/// SLSA provenance: a **DSSE attestation**, which `ocx package attest` publishes
/// under the very same [`SIGSTORE_BUNDLE_V03`] artifactType a signature carries.
///
/// Derived from the signature capture rather than hand-built, so the two differ
/// in exactly the one field that separates them — same verificationMaterial,
/// same envelope shape, same artifactType, same annotations. A lister that
/// discriminated on anything else would keep this candidate and red the two
/// tests below.
fn attestation_bundle() -> Vec<u8> {
    use base64::Engine as _;

    let base64 = base64::engine::general_purpose::STANDARD;
    let mut bundle: serde_json::Value = serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("golden bundle is JSON");
    let encoded = bundle
        .pointer("/dsseEnvelope/payload")
        .and_then(serde_json::Value::as_str)
        .expect("the capture carries a DSSE payload");
    let mut statement: serde_json::Value =
        serde_json::from_slice(&base64.decode(encoded).expect("the DSSE payload is base64"))
            .expect("the DSSE payload is an in-toto Statement");
    statement["predicateType"] = serde_json::Value::String("https://slsa.dev/provenance/v1".to_string());
    bundle["dsseEnvelope"]["payload"] =
        serde_json::Value::String(base64.encode(serde_json::to_vec(&statement).expect("the Statement re-serializes")));
    serde_json::to_vec(&bundle).expect("the bundle re-serializes")
}

/// Parks `bytes` at a sibling tag of [`image`], the way a cosign sidecar or a
/// fallback index sits beside the subject it belongs to.
fn seed_tag(data: &StubTransportData, tag: String, bytes: &[u8]) {
    let target = sibling_tag_reference(&image(), tag);
    data.write().manifests.insert(
        target.to_string(),
        (bytes.to_vec(), Algorithm::Sha256.hash(bytes).to_string()),
    );
}

/// A fallback referrers index holding `entries`, serialized as a registry
/// would store it at `sha256-<hex>`.
fn fallback_index(entries: Vec<oci::ImageIndexEntry>) -> Vec<u8> {
    let index = oci::ImageIndex {
        schema_version: oci::INDEX_SCHEMA_VERSION,
        media_type: Some(crate::media_type::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
        manifests: entries,
        artifact_type: None,
        annotations: None,
    };
    serde_json::to_vec(&index).expect("fallback index serializes")
}

fn index_entry(digest: String, size: i64, artifact_type: Option<&str>) -> oci::ImageIndexEntry {
    oci::ImageIndexEntry {
        media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
        digest,
        size,
        platform: None,
        annotations: None,
        artifact_type: artifact_type.map(str::to_string),
    }
}

async fn candidates(data: &StubTransportData) -> Result<Vec<SignerCandidate>, ClientError> {
    let transport = StubTransport::new(data.clone());
    list_signature_candidates(&transport, &image(), &subject()).await
}

/// The one candidate a listing must have produced, or a panic naming what it
/// produced instead.
fn only(found: Vec<SignerCandidate>) -> SignerCandidate {
    assert_eq!(found.len(), 1, "expected exactly one candidate, got: {found:?}");
    found.into_iter().next().expect("length was just asserted")
}

// ── OCX-C-2: absence, and the one error path ───────────────────────────

/// **OCX-C-2.** A subject with nothing attached lists an empty vector, never
/// an error.
///
/// "Nothing found" and "could not look" are different answers and the backfill
/// acts on them differently: the first means *sign this*, the second means
/// *stop*. Collapsing the first into an error would make every unsigned
/// subject look like a registry fault; collapsing the second into an empty
/// vector would sign a subject that is already signed.
#[tokio::test]
async fn no_signature_anywhere_is_an_empty_vector_not_an_error() {
    let data = StubTransportData::new();

    let found = candidates(&data).await.expect("an unsigned subject is not a failure");

    assert!(
        found.is_empty(),
        "nothing is attached, so nothing is a candidate: {found:?}"
    );
}

/// **OCX-C-2.** A sidecar read that fails for any reason other than an absent
/// tag is an `Err`, not an empty vector.
///
/// The referrers half answers cleanly here, so this isolates the sidecar door:
/// a reader that mapped every sidecar failure to "no sidecar" would return
/// `Ok(vec![])` and pass every test above.
#[tokio::test]
async fn a_failing_sidecar_read_is_an_error_not_an_absent_sidecar() {
    let data = StubTransportData::new();
    // Fires for any image not in `manifests`, which is exactly the `.sig` tag.
    data.write().pull_manifest_error_override = Some("registry is unreachable".to_string());

    let error = candidates(&data)
        .await
        .expect_err("a registry that could not answer must not read as 'not signed'");

    assert!(
        matches!(error, ClientError::Registry(_)),
        "the transport's own error must surface unchanged, got: {error:?}"
    );
}

/// **OCX-C-2.** The same rule on the referrer half: a fallback-index read that
/// fails is an `Err`.
///
/// The sibling of the test above rather than a merge with it, because the two
/// doors fail through different call paths and a reader can swallow one while
/// propagating the other.
#[tokio::test]
async fn a_failing_fallback_index_read_is_an_error() {
    let data = StubTransportData::new();
    data.write().referrers_unsupported = true;
    data.write().pull_manifest_error_override = Some("registry is unreachable".to_string());

    let error = candidates(&data)
        .await
        .expect_err("an unreadable fallback index must not read as 'no referrers'");

    assert!(
        matches!(error, ClientError::Registry(_)),
        "the transport's own error must surface unchanged, got: {error:?}"
    );
}

// ── OCX-C-2: the three discovery methods ───────────────────────────────

/// **OCX-C-2.** A signature-typed referrer served by the native Referrers API
/// lists as one candidate reporting `ReferrersApi`, the referrer's digest and
/// its `artifactType`.
#[tokio::test]
async fn a_native_signature_referrer_lists_with_referrers_api_discovery() {
    let data = StubTransportData::new();
    let referrer_digest = seed_referrer(&data, SIGSTORE_BUNDLE_V03, GOLDEN_KEYLESS_BUNDLE.as_bytes()).await;

    let candidate = only(candidates(&data).await.expect("the listing succeeds"));

    assert_eq!(
        candidate.discovery,
        DiscoveryMethod::ReferrersApi,
        "a registry-computed answer must not be reported as a mutable tag"
    );
    assert_eq!(
        candidate.digest.to_string(),
        referrer_digest,
        "the candidate must name the referrer manifest, not the subject or the bundle blob"
    );
    assert_eq!(candidate.artifact_type.as_deref(), Some(SIGSTORE_BUNDLE_V03));
}

/// **OCX-C-2.** The same signature reached through the `sha256-<hex>` fallback
/// tag reports `FallbackTag`.
///
/// The referrer is pushed while the stub still serves the Referrers API, then
/// the API is switched off — so the manifest store holds exactly what a
/// registry without the API would hold, and only the discovery route differs
/// from the test above. Reporting `ReferrersApi` here would tell the mirror a
/// mutable tag anyone with push access authored was a registry-computed answer.
#[tokio::test]
async fn the_same_signature_through_the_fallback_tag_reports_fallback_tag() {
    let data = StubTransportData::new();
    let referrer_digest = seed_referrer(&data, SIGSTORE_BUNDLE_V03, GOLDEN_KEYLESS_BUNDLE.as_bytes()).await;
    let referrer_bytes = referrer_manifest(&subject(), SIGSTORE_BUNDLE_V03, GOLDEN_KEYLESS_BUNDLE.as_bytes());
    seed_tag(
        &data,
        crate::package::tag::referrer_fallback_tag(&subject()),
        &fallback_index(vec![index_entry(
            referrer_digest.clone(),
            referrer_bytes.len() as i64,
            Some(SIGSTORE_BUNDLE_V03),
        )]),
    );
    data.write().referrers_unsupported = true;

    let candidate = only(candidates(&data).await.expect("the listing succeeds"));

    assert_eq!(candidate.discovery, DiscoveryMethod::FallbackTag);
    assert_eq!(candidate.digest.to_string(), referrer_digest);
    assert_eq!(candidate.artifact_type.as_deref(), Some(SIGSTORE_BUNDLE_V03));
}

/// **OCX-C-2.** A cosign `sha256-<hex>.sig` sidecar lists as one candidate
/// reporting `SidecarTag`.
///
/// This door needs no Referrers API at all, which is why it is a door and not
/// a filter over the listing: a registry serving the API and holding no
/// referrers still holds this tag.
#[tokio::test]
async fn a_sig_sidecar_tag_lists_with_sidecar_tag_discovery() {
    let data = StubTransportData::new();
    let sidecar = GOLDEN_SIMPLESIGNING_MANIFEST.as_bytes();
    seed_tag(&data, sidecar_tag(&subject(), SidecarKind::Signature), sidecar);

    let candidate = only(candidates(&data).await.expect("the listing succeeds"));

    assert_eq!(candidate.discovery, DiscoveryMethod::SidecarTag);
    assert_eq!(
        candidate.digest.to_string(),
        Algorithm::Sha256.hash(sidecar).to_string(),
        "a sidecar candidate is addressed by its manifest digest"
    );
}

// ── OCX-C-2: what is NOT a signature ───────────────────────────────────

/// **OCX-C-2, the 2026-09-02 narrowing.** `.att` and `.sbom` sidecars, and
/// referrers whose `artifactType` is not a signature, are not candidates.
///
/// This is the arm the whole contract turns on. An attestation and an SBOM are
/// attached to plenty of unsigned subjects, so a lister that counted them
/// would report "already has something" for a subject carrying **no
/// signature**, and the mirror's backfill would skip exactly the artifacts it
/// exists to sign. Everything below is seeded at once so the test cannot pass
/// by excluding one class and admitting another.
#[tokio::test]
async fn attestation_and_sbom_attachments_are_not_signature_candidates() {
    let data = StubTransportData::new();
    // An SBOM referrer: cosign's own OCI 1.1 spelling, which carries an
    // artifactType and would otherwise satisfy a "has a referrer" filter.
    seed_referrer(&data, COSIGN_SBOM_ARTIFACT_TYPE, b"{}").await;
    // An attestation referrer. cosign publishes no attestation artifact type
    // (see `referrer/media_types.rs`), so this stands for any non-signature
    // type a foreign tool may attach — the open half of the set.
    seed_referrer(&data, "application/vnd.example.attestation.v1+json", b"{}").await;
    seed_tag(
        &data,
        sidecar_tag(&subject(), SidecarKind::Attestation),
        GOLDEN_SIMPLESIGNING_MANIFEST.as_bytes(),
    );
    seed_tag(
        &data,
        crate::package::tag::sbom_sidecar_tag(&subject()),
        GOLDEN_SIMPLESIGNING_MANIFEST.as_bytes(),
    );

    let found = candidates(&data).await.expect("the listing succeeds");

    assert!(
        found.is_empty(),
        "an attestation or SBOM is not a signature, and counting one lets the backfill \
         skip an unsigned subject: {found:?}"
    );
}

/// **OCX-C-2, the attestation exclusion.** A subject carrying only a DSSE
/// attestation lists **no** candidates, even though its referrer wears the
/// signature artifact type.
///
/// `attest/pipeline.rs` writes [`SIGSTORE_BUNDLE_V03`] on an attestation
/// referrer and `sign/pipeline.rs` writes `dsse-envelope` as the bundle-content
/// annotation on a signature, so neither producer-controlled field separates
/// the two: only the bundle's own Statement does. Without that check this
/// subject reports one signature and the mirror's backfill skips an artifact
/// nobody ever signed — the same failure the `.att` sidecar arm above prevents,
/// reached through the referrers door instead.
#[tokio::test]
async fn an_attestation_referrer_under_the_signature_artifact_type_is_not_a_candidate() {
    let data = StubTransportData::new();
    seed_referrer(&data, SIGSTORE_BUNDLE_V03, &attestation_bundle()).await;

    let found = candidates(&data).await.expect("the listing succeeds");

    assert!(
        found.is_empty(),
        "an attestation shares the signature artifactType, so only its bundle can \
         exclude it; counting one lets the backfill skip an unsigned subject: {found:?}"
    );
}

/// **OCX-C-2, the attestation exclusion.** A subject carrying one signature and
/// one attestation lists exactly one candidate: the signature.
///
/// The sibling of the test above, and not redundant with it: a lister that
/// dropped *every* bundle referrer passes that one and fails this, and one that
/// discriminated on the artifactType or the referrer's annotations passes this
/// by keeping both. The digest assertion is what names which of the two
/// survived — a count alone would accept the wrong one.
#[tokio::test]
async fn a_signature_and_an_attestation_yield_only_the_signature() {
    let data = StubTransportData::new();
    let signature_digest = seed_referrer(&data, SIGSTORE_BUNDLE_V03, GOLDEN_KEYLESS_BUNDLE.as_bytes()).await;
    seed_referrer(&data, SIGSTORE_BUNDLE_V03, &attestation_bundle()).await;

    let candidate = only(candidates(&data).await.expect("the listing succeeds"));

    assert_eq!(
        candidate.digest.to_string(),
        signature_digest,
        "the surviving candidate must be the signature referrer, not the attestation"
    );
    assert_eq!(
        candidate.certificate_identity.as_deref(),
        Some(GOLDEN_IDENTITY),
        "the exclusion must not cost the signature its identity fields"
    );
}

// ── OCX-C-2: the unvalidated identity fields ───────────────────────────

/// **OCX-C-2.** The identity fields are read off a keyless bundle whose chain
/// was never checked.
///
/// The assertion is that they are *populated with what the bundle claims* —
/// never that the claim is true. `certificate_identity` and
/// `certificate_issuer` are documented unvalidated for exactly this reason: a
/// caller matching on them is trusting whoever could write to the registry,
/// and `VerifyPipeline` remains the only answer to "is this signature good".
#[tokio::test]
async fn a_keyless_bundle_yields_its_unvalidated_certificate_identity_and_issuer() {
    let data = StubTransportData::new();
    seed_referrer(&data, SIGSTORE_BUNDLE_V03, GOLDEN_KEYLESS_BUNDLE.as_bytes()).await;

    let candidate = only(candidates(&data).await.expect("the listing succeeds"));

    assert_eq!(
        candidate.certificate_identity.as_deref(),
        Some(GOLDEN_IDENTITY),
        "the SAN the local stack minted must reach the caller"
    );
    assert_eq!(candidate.certificate_issuer.as_deref(), Some(GOLDEN_ISSUER));
    assert_eq!(
        candidate.public_key_hint, None,
        "a keyless bundle carries no publicKey, so a hint here would be invented"
    );
}

/// **OCX-C-2.** A key-mode bundle yields `public_key_hint` and leaves both
/// certificate fields `None`.
///
/// The hint is read out of the fixture rather than transcribed, so a
/// regenerated capture moves the expectation with it instead of silently
/// disagreeing. Unlike the certificate fields the hint is self-authenticating
/// against a configured key: a forged one only yields a signature that then
/// fails verification.
#[tokio::test]
async fn a_key_bundle_yields_a_public_key_hint_and_no_certificate_fields() {
    let bundle: serde_json::Value = serde_json::from_str(GOLDEN_KEY_BUNDLE).expect("golden key bundle is JSON");
    let hint = bundle
        .pointer("/verificationMaterial/publicKey/hint")
        .and_then(serde_json::Value::as_str)
        .expect("the key-mode capture carries a publicKey hint")
        .to_owned();

    let data = StubTransportData::new();
    seed_referrer(&data, SIGSTORE_BUNDLE_V03, GOLDEN_KEY_BUNDLE.as_bytes()).await;

    let candidate = only(candidates(&data).await.expect("the listing succeeds"));

    assert_eq!(candidate.public_key_hint.as_deref(), Some(hint.as_str()));
    assert_eq!(
        candidate.certificate_identity, None,
        "a key-mode bundle has no certificate, so an identity here would be invented"
    );
    assert_eq!(candidate.certificate_issuer, None);
}

// ── OCX-C-2: the fallback bound is the transport's ─────────────────────

/// **OCX-C-2.** The fallback path is `list_referrers_with_fallback`, so the
/// descriptor ceiling that method already enforces still applies.
///
/// Both halves are asserted because either alone is satisfiable by a wrong
/// implementation: a lister that hand-rolled the fallback read past the
/// transport would pass the ceiling case and fail the refusal, and one that
/// refused every fallback index would pass the refusal and fail the ceiling.
/// The entries are typed as a non-signature artifact so no manifest is
/// fetched — the bound under test is the index decode, which runs before any
/// filtering.
#[tokio::test]
async fn the_fallback_descriptor_ceiling_is_the_transports_and_is_not_bypassed() {
    let entries = |count: usize| {
        (0..count)
            .map(|n| index_entry(format!("sha256:{n:064x}"), 2, Some(COSIGN_SBOM_ARTIFACT_TYPE)))
            .collect::<Vec<_>>()
    };

    let at_ceiling = StubTransportData::new();
    at_ceiling.write().referrers_unsupported = true;
    seed_tag(
        &at_ceiling,
        crate::package::tag::referrer_fallback_tag(&subject()),
        &fallback_index(entries(TRANSPORT_FALLBACK_DESCRIPTOR_CEILING)),
    );
    let found = candidates(&at_ceiling)
        .await
        .expect("an index at the ceiling is readable, and holds no signature");
    assert!(found.is_empty(), "SBOM entries are not signature candidates: {found:?}");

    let past_ceiling = StubTransportData::new();
    past_ceiling.write().referrers_unsupported = true;
    seed_tag(
        &past_ceiling,
        crate::package::tag::referrer_fallback_tag(&subject()),
        &fallback_index(entries(TRANSPORT_FALLBACK_DESCRIPTOR_CEILING + 1)),
    );
    let error = candidates(&past_ceiling)
        .await
        .expect_err("an index past the ceiling must be refused, not truncated into a listing");
    assert!(
        matches!(error, ClientError::InvalidManifest(_)),
        "the refusal must be the transport's own, got: {error:?}"
    );
}

/// **OCX-C-2.** A signature-typed descriptor whose digest is not a digest is
/// dropped, and the sound candidates beside it still list.
///
/// The registry answered, so this is not the "could not look" case an `Err` is
/// reserved for — and a lister that propagated it would let one malformed
/// sibling abandon the whole subject, which is neither of the two answers the
/// backfill knows how to act on. The valid entry is asserted present rather
/// than only asserting `Ok`, because a lister that gave up at the bad
/// descriptor and returned what it had so far would pass a bare `is_ok`.
#[tokio::test]
async fn a_descriptor_with_an_unparseable_digest_is_dropped_not_an_error() {
    let data = StubTransportData::new();
    let referrer_digest = seed_referrer(&data, SIGSTORE_BUNDLE_V03, GOLDEN_KEYLESS_BUNDLE.as_bytes()).await;
    let referrer_bytes = referrer_manifest(&subject(), SIGSTORE_BUNDLE_V03, GOLDEN_KEYLESS_BUNDLE.as_bytes());
    seed_tag(
        &data,
        crate::package::tag::referrer_fallback_tag(&subject()),
        &fallback_index(vec![
            index_entry("sha256:not-a-digest".to_string(), 2, Some(SIGSTORE_BUNDLE_V03)),
            index_entry(
                referrer_digest.clone(),
                referrer_bytes.len() as i64,
                Some(SIGSTORE_BUNDLE_V03),
            ),
        ]),
    );
    data.write().referrers_unsupported = true;

    let candidate = only(
        candidates(&data)
            .await
            .expect("a malformed descriptor is the registry answering badly, not failing to answer"),
    );

    assert_eq!(
        candidate.digest.to_string(),
        referrer_digest,
        "the sound referrer beside the malformed one must still list"
    );
}

/// **OCX-C-2.** The candidate ceiling is `MAX_SIGNATURE_CANDIDATES`, applied to
/// the signature-typed referrers themselves.
///
/// Distinct from the fallback-descriptor ceiling above: that one bounds the
/// index *decode* and fires before any filtering, so an implementation with no
/// candidate cap at all passes it. This seeds one more signature referrer than
/// the ceiling through the native Referrers API — no fallback index in play —
/// so the only thing that can hold the count down is the cap in
/// `list_signature_candidates` itself.
///
/// The bundles are distinct junk rather than the golden capture: each must be
/// a *separate* referrer manifest to count, and a bundle that does not parse
/// still yields a candidate with empty identity fields, which is exactly the
/// cheapest shape that exercises the cap.
#[tokio::test]
async fn more_signature_referrers_than_the_ceiling_list_only_the_ceiling() {
    let data = StubTransportData::new();
    for n in 0..=MAX_SIGNATURE_CANDIDATES {
        seed_referrer(&data, SIGSTORE_BUNDLE_V03, format!("{{\"n\":{n}}}").as_bytes()).await;
    }

    let found = candidates(&data).await.expect("the listing succeeds");

    assert_eq!(
        found.len(),
        MAX_SIGNATURE_CANDIDATES,
        "a registry chooses how long its own referrers listing is; every entry past \
         the ceiling costs a manifest and a blob fetch: {found:?}"
    );
}

// ── OCX-C-1: the factory, and the copy path it must not touch ──────────

/// **OCX-C-1.** `native_transport` is nameable with the ratified signature and
/// hands back a `Send + Sync` transport.
///
/// A coercion to a function pointer rather than a call: the signature is the
/// contract, and building a fork client and an `Auth` to invoke it would
/// assert nothing this does not. `Send + Sync` is what lets the mirror hold
/// the returned transport across its own fan-out, and it comes from the
/// trait's own supertrait bounds — asserted rather than assumed, because
/// relaxing them would compile everywhere except a caller like that.
///
/// What this **cannot** prove from inside `ocx_lib` is that the function is
/// `pub` rather than `pub(crate)`; both spellings satisfy this. That half is
/// proven by an out-of-crate consumer, which is the lead repository's WP 9
/// gate (`cargo check` with `native_transport` nameable).
#[test]
fn native_transport_is_nameable_with_the_ratified_signature() {
    const _FACTORY: fn(oci::native::Client, crate::auth::Auth) -> Box<dyn OciTransport> =
        crate::oci::client::native_transport;

    const fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<Box<dyn OciTransport>>();
}

/// **OCX-C-1.** `oci/copy.rs` still refuses a referrers-less target and never
/// writes a fallback index.
///
/// A source scan, deliberately: the property is the *absence* of a call, and
/// there is no behaviour to observe when a function is never invoked. The
/// refusal half already has behavioural cover in `copy.rs`'s own tests
/// (`the_sidecar_sweep_runs_before_the_referrers_gate` and its two siblings
/// asserting `ClientError::ReferrersUnsupported`); this guard exists for the
/// half those cannot express, and re-asserts the refusal's presence only so a
/// gutted `copy.rs` cannot pass by having nothing left to find.
///
/// Comments are stripped before scanning, so a future doc comment mentioning
/// the forbidden call cannot red this, and the two positive controls are what
/// stop a scan that matched nothing from reading as a pass.
#[test]
fn the_copy_path_never_appends_to_the_fallback_index() {
    const COPY_RS: &str = include_str!("../../copy.rs");

    // Line comments only: `copy.rs` carries no block comments, and a `//`
    // inside a string literal (a URL) can only over-strip, which cannot turn a
    // finding into a pass — every needle below is a Rust path, never a URL.
    let code: String = COPY_RS
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(before, _)| before))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("ClientError::ReferrersUnsupported"),
        "positive control: the exit-84 refusal must still be constructed in copy.rs, \
         or this scan is reading a file that no longer holds the contract"
    );
    assert!(
        code.contains("list_referrers_with_fallback"),
        "positive control: copy.rs must still READ referrers with the fallback — \
         the read is allowed, only the write is not"
    );
    assert_eq!(
        code.matches("append_referrer_fallback_index").count(),
        0,
        "`ocx package copy` refuses a referrers-less target by ratified decision \
         (ocx#392) and must never write the fallback index instead; the transport \
         factory (OCX-C-1) is how ocx-mirror gets that write, not this path"
    );
}

// ── the out-of-crate constructor ───────────────────────────────────────

/// `SignerCandidate::new` plus the `with_*` setters round-trip every field.
///
/// The type is `#[non_exhaustive]`, so a downstream crate cannot write a
/// struct literal — this constructor is the whole of its construction
/// contract, and `ocx-mirror`'s `--identity` / `--issuer` filter tests are
/// what consume it. Asserted here from inside the crate because a doctest
/// covers the shape but not the fields left alone: `artifact_type` and
/// `public_key_hint` must stay `None` when nothing set them, or a filter
/// would match on a value the constructor invented.
#[test]
fn a_candidate_built_through_the_public_constructor_round_trips_its_fields() {
    let digest = Algorithm::Sha256.hash(b"bundle");

    let candidate = SignerCandidate::new(DiscoveryMethod::ReferrersApi, digest.clone())
        .with_artifact_type(SIGSTORE_BUNDLE_V03)
        .with_certificate_identity(GOLDEN_IDENTITY)
        .with_certificate_issuer(GOLDEN_ISSUER);

    assert_eq!(candidate.discovery, DiscoveryMethod::ReferrersApi);
    assert_eq!(candidate.digest, digest);
    assert_eq!(candidate.artifact_type.as_deref(), Some(SIGSTORE_BUNDLE_V03));
    assert_eq!(candidate.certificate_identity.as_deref(), Some(GOLDEN_IDENTITY));
    assert_eq!(candidate.certificate_issuer.as_deref(), Some(GOLDEN_ISSUER));
    assert_eq!(
        candidate.public_key_hint, None,
        "an unset field must stay unset — a keyless candidate carries no hint"
    );

    let key_mode = SignerCandidate::new(DiscoveryMethod::SidecarTag, digest).with_public_key_hint("abcd");
    assert_eq!(key_mode.public_key_hint.as_deref(), Some("abcd"));
    assert_eq!(
        key_mode.certificate_identity, None,
        "the certificate fields must stay unset when only the hint was set"
    );
    assert_eq!(key_mode.artifact_type, None);
}
