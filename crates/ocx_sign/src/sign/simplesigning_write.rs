// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Writing a cosign sidecar tag — `sha256-<hex>.sig` and `sha256-<hex>.att`.
//!
//! Re-signing appends, never replaces: replacing would delete a signature someone else published.

use std::collections::BTreeMap;

use serde::Serialize;

use super::error::SignErrorKind;
use super::referrers::map_client_error;
use super::rekor::RekorEntry;
use super::signer::SignedBlob;
use crate::verify::SidecarKind;
use ocx_oci::client::OciTransport;
use ocx_oci::client::error::ClientError;
use ocx_oci::referrer::media_types::{
    ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE, ANNOTATION_COSIGN_CHAIN, ANNOTATION_COSIGN_SIGNATURE,
    DSSE_ENVELOPE_MEDIA_TYPE, EMPTY_CONFIG, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_PAYLOAD, EMPTY_CONFIG_SIZE,
    SIMPLESIGNING_MEDIA_TYPE,
};
use ocx_oci::{Algorithm, Descriptor, Digest, ImageManifest, OCI_IMAGE_MEDIA_TYPE, native};

/// Bounded so a concurrent-writer race becomes a loud, retryable failure, never a lost signature.
const MAX_APPEND_ATTEMPTS: usize = 5;

/// Layers one sidecar manifest may carry; unbounded, anyone with push access can make verification arbitrarily expensive.
const MAX_SIDECAR_LAYERS: usize = 32;

/// One sidecar layer, ready to append.
///
/// Built only by the two constructors, or a swapped kind/media type publishes a layer no cosign reader accepts.
pub(crate) struct SidecarLayer {
    /// The reader's enum: the tag is spelled once, in [`crate::verify::sidecar_tag`].
    kind: SidecarKind,
    media_type: &'static str,
    payload: Vec<u8>,
    annotations: BTreeMap<String, String>,
}

impl SidecarLayer {
    /// The `sha256-<hex>.sig` layer; `payload` is the exact claim bytes the signature covers.
    pub(crate) fn signature(payload: Vec<u8>, signed: &SignedBlob) -> Self {
        use base64::Engine as _;
        let base64 = base64::engine::general_purpose::STANDARD;

        let mut annotations = BTreeMap::new();
        // The signature is detached from the payload, so this annotation is where a verifier finds it.
        annotations.insert(
            ANNOTATION_COSIGN_SIGNATURE.to_string(),
            base64.encode(&signed.signature),
        );
        insert_present(
            &mut annotations,
            signed.certificate_pem.as_deref(),
            signed.chain_pem.as_deref(),
            signed.rekor_bundle.as_deref(),
        );
        Self {
            kind: SidecarKind::Signature,
            media_type: SIMPLESIGNING_MEDIA_TYPE,
            payload,
            annotations,
        }
    }

    /// The `sha256-<hex>.att` layer for one DSSE envelope, published bare.
    ///
    /// The signature annotation is written present and empty: `cosign verify-attestation` refuses a layer without it.
    pub(crate) fn attestation(envelope: Vec<u8>, certificate_pem: Option<&str>, rekor_bundle: Option<&str>) -> Self {
        let mut annotations = BTreeMap::new();
        annotations.insert(ANNOTATION_COSIGN_SIGNATURE.to_string(), String::new());
        insert_present(&mut annotations, certificate_pem, None, rekor_bundle);
        Self {
            kind: SidecarKind::Attestation,
            media_type: DSSE_ENVELOPE_MEDIA_TYPE,
            payload: envelope,
            annotations,
        }
    }

    fn descriptor(&self) -> Descriptor {
        Descriptor {
            media_type: self.media_type.to_string(),
            digest: Algorithm::Sha256.hash(&self.payload).to_string(),
            size: self.payload.len() as i64,
            annotations: Some(self.annotations.clone()),
            ..Descriptor::default()
        }
    }
}

/// Insert the verification material cosign reads out of layer annotations.
///
/// Absent material is omitted, never empty, or a reader sees present-but-broken material.
fn insert_present(
    annotations: &mut BTreeMap<String, String>,
    certificate_pem: Option<&str>,
    chain_pem: Option<&str>,
    rekor_bundle: Option<&str>,
) {
    for (key, value) in [
        (ANNOTATION_COSIGN_CERTIFICATE, certificate_pem),
        (ANNOTATION_COSIGN_CHAIN, chain_pem),
        (ANNOTATION_COSIGN_BUNDLE, rekor_bundle),
    ] {
        if let Some(value) = value {
            annotations.insert(key.to_string(), value.to_owned());
        }
    }
}

/// Append `layer` to the sidecar manifest at its tag, creating the manifest when absent.
///
/// `image` must be the write reference: a signature written to a mirror is never seen by verifiers.
///
/// # Errors
///
/// The push's error; [`ClientError::RegistryTransient`] when concurrent writers exhaust [`MAX_APPEND_ATTEMPTS`];
/// [`ClientError::InvalidManifest`] when the sidecar is full. Both wrapped in [`SignErrorKind::Internal`].
pub(crate) async fn append_layer(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject: &Digest,
    layer: &SidecarLayer,
) -> Result<Digest, SignErrorKind> {
    let payload = layer.payload.as_slice();
    let tag = crate::verify::sidecar_tag(subject, layer.kind);
    let target = ocx_oci::client::sibling_tag_reference(image, tag.clone());
    let layer = layer.descriptor();

    let no_progress: std::sync::Arc<dyn Fn(u64) + Send + Sync> = std::sync::Arc::new(|_| ());
    let payload_digest = Algorithm::Sha256.hash(payload);
    transport
        .push_blob(image, payload.to_vec(), &payload_digest, no_progress.clone())
        .await
        .map_err(map_client_error)?;
    let empty_config_digest =
        Digest::try_from(EMPTY_CONFIG_DIGEST).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    transport
        .push_blob(image, EMPTY_CONFIG_PAYLOAD.to_vec(), &empty_config_digest, no_progress)
        .await
        .map_err(map_client_error)?;

    for _attempt in 0..MAX_APPEND_ATTEMPTS {
        let published = read_sidecar(transport, &target).await?;
        let existing = published
            .as_ref()
            .map_or_else(empty_sidecar, |(manifest, _)| manifest.clone());
        if let Some((_, served_digest)) = &published
            && existing.layers.iter().any(|held| is_same_layer(held, &layer))
        {
            // Report the served digest, never a re-serialization: serde may not reproduce the published bytes.
            return Ok(served_digest.clone());
        }
        if existing.layers.len() >= MAX_SIDECAR_LAYERS {
            log::warn!(
                "cosign sidecar {tag} already holds {} layers; appending would pass the \
                 {MAX_SIDECAR_LAYERS} limit",
                existing.layers.len()
            );
            return Err(SignErrorKind::Internal(Box::new(ClientError::InvalidManifest(
                format!(
                    "cosign sidecar {tag} holds {} layers, the limit is {MAX_SIDECAR_LAYERS}",
                    existing.layers.len()
                ),
            ))));
        }

        // Rebuilt from parsed values, never echoed: we re-publish a stranger's manifest under our credentials.
        let next = rebuild_with(existing, layer.clone());
        let bytes = serde_json::to_vec(&next).map_err(serialization)?;
        let digest = Algorithm::Sha256.hash(&bytes);
        transport
            .push_manifest_raw(&target, bytes, OCI_IMAGE_MEDIA_TYPE)
            .await
            .map_err(map_client_error)?;

        // The only evidence the PUT survived: a concurrent writer's PUT returns `Ok` to us and drops our layer.
        let after = read_sidecar(transport, &target).await?;
        if after.is_some_and(|(manifest, _)| manifest.layers.iter().any(|held| is_same_layer(held, &layer))) {
            return Ok(digest);
        }
    }

    Err(SignErrorKind::Internal(Box::new(ClientError::RegistryTransient(
        format!(
            "cosign sidecar {tag} was overwritten by a concurrent writer {MAX_APPEND_ATTEMPTS} times; \
             the layer was not appended"
        )
        .into(),
    ))))
}

/// Whether `held` is the layer `layer` carries, not merely a layer at the same address.
///
/// `.sig` claim bytes repeat across signers, so the signature annotation must match too, or a clobber passes the read-back.
/// An unannotated `.att` is a different layer: no cosign release verifies it.
fn is_same_layer(held: &Descriptor, layer: &Descriptor) -> bool {
    // Only the signature annotation: optional material may differ between re-appends.
    held.digest == layer.digest && signature_annotation(held) == signature_annotation(layer)
}

fn signature_annotation(descriptor: &Descriptor) -> Option<&String> {
    descriptor.annotations.as_ref()?.get(ANNOTATION_COSIGN_SIGNATURE)
}

/// Read the sidecar manifest at `target`, and its digest; `None` when the tag does not exist.
///
/// Only a missing tag is `Ok(None)`: treating an unreadable one alike republishes over others' signatures.
async fn read_sidecar(
    transport: &dyn OciTransport,
    target: &native::Reference,
) -> Result<Option<(ImageManifest, Digest)>, SignErrorKind> {
    match transport
        .pull_manifest_raw(target, ocx_oci::media_type::ACCEPTED_MANIFEST_MEDIA_TYPES)
        .await
    {
        Ok((bytes, _served_digest)) => {
            let manifest: ImageManifest = serde_json::from_slice(&bytes).map_err(|error| {
                SignErrorKind::Internal(Box::new(ClientError::InvalidManifest(format!(
                    "simplesigning sidecar is not an image manifest: {error}"
                ))))
            })?;
            // Hashed from the served bytes, never a re-serialization.
            Ok(Some((manifest, Algorithm::Sha256.hash(&bytes))))
        }
        Err(ClientError::ManifestNotFound(_)) => Ok(None),
        Err(other) => Err(map_client_error(other)),
    }
}

/// The manifest a sidecar tag starts from when nothing is published there.
fn empty_sidecar() -> ImageManifest {
    ImageManifest {
        schema_version: 2,
        media_type: Some(OCI_IMAGE_MEDIA_TYPE.to_string()),
        config: Descriptor {
            media_type: EMPTY_CONFIG.to_string(),
            digest: EMPTY_CONFIG_DIGEST.to_string(),
            size: EMPTY_CONFIG_SIZE as i64,
            ..Descriptor::default()
        },
        layers: Vec::new(),
        ..ImageManifest::default()
    }
}

/// Re-emit `existing` with `layer` appended, field by field.
///
/// Keep the config: cosign's `.sig` points at a real image config, and resetting it orphans what cosign published.
fn rebuild_with(existing: ImageManifest, layer: Descriptor) -> ImageManifest {
    let mut next = empty_sidecar();
    next.config = Descriptor {
        media_type: existing.config.media_type,
        digest: existing.config.digest,
        size: existing.config.size,
        ..Descriptor::default()
    };
    next.layers = existing
        .layers
        .into_iter()
        .map(|held| Descriptor {
            media_type: held.media_type,
            digest: held.digest,
            size: held.size,
            urls: held.urls,
            artifact_type: held.artifact_type,
            annotations: held.annotations,
        })
        .collect();
    next.layers.push(layer);
    next
}

/// The `dev.sigstore.cosign/bundle` annotation value for `entry`; the capitalized names are cosign's Go wire tags.
///
/// # Errors
///
/// [`SignErrorKind::Internal`] if the value cannot be serialized.
pub(super) fn offline_bundle(entry: &RekorEntry) -> Result<String, SignErrorKind> {
    use base64::Engine as _;
    let base64 = base64::engine::general_purpose::STANDARD;

    serde_json::to_string(&OfflineBundle {
        signed_entry_timestamp: base64.encode(&entry.signed_entry_timestamp),
        payload: OfflineBundlePayload {
            body: base64.encode(&entry.canonicalized_body),
            integrated_time: entry.integrated_time,
            log_index: entry.log_index,
            log_id: entry.log_id.clone(),
        },
    })
    .map_err(serialization)
}

/// cosign's offline Rekor bundle, as it appears in the annotation.
#[derive(Serialize)]
struct OfflineBundle {
    #[serde(rename = "SignedEntryTimestamp")]
    signed_entry_timestamp: String,
    #[serde(rename = "Payload")]
    payload: OfflineBundlePayload,
}

#[derive(Serialize)]
struct OfflineBundlePayload {
    body: String,
    #[serde(rename = "integratedTime")]
    integrated_time: u64,
    #[serde(rename = "logIndex")]
    log_index: u64,
    #[serde(rename = "logID")]
    log_id: String,
}

fn serialization(error: serde_json::Error) -> SignErrorKind {
    SignErrorKind::Internal(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::sidecar_tag;
    use ocx_oci::client::Client;
    use ocx_oci::client::sibling_tag_reference;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use ocx_oci::{DEFAULT_REGISTRY, OciIdentifier};

    /// The subject whose signatures these tests append to.
    fn subject() -> Digest {
        Algorithm::Sha256.hash(b"subject manifest")
    }

    /// A client over `data`, the write reference `append_layer` requires, and
    /// the `.sig` tag it derives from that reference.
    ///
    /// The reference comes from [`Client::transport_write_reference`] because
    /// that is what this function's own doc demands ("`image` **must** be the
    /// write reference"), and a test that hand-built one could drift from the
    /// seam the production caller goes through. The tag is derived with the
    /// same two helpers `append_layer` uses, so a test can never key its stub
    /// on a tag the code does not address.
    fn client_and_sidecar_key(data: &StubTransportData) -> (Client, native::Reference, String) {
        let client = Client::with_transport(Box::new(StubTransport::new(data.clone())));
        let image = client.transport_write_reference(
            &OciIdentifier::parse_target("registry.example/team/pkg:1.0", DEFAULT_REGISTRY).expect("test identifier"),
        );
        let key = sibling_tag_reference(&image, sidecar_tag(&subject(), SidecarKind::Signature)).to_string();
        (client, image, key)
    }

    /// The layer under test: a `.sig` layer over a fixed claim.
    fn our_layer() -> SidecarLayer {
        SidecarLayer::signature(
            b"{\"critical\":{}}".to_vec(),
            &SignedBlob {
                signature: b"ours".to_vec(),
                certificate_pem: None,
                chain_pem: None,
                rekor_bundle: None,
                transparency_log_index: None,
                key_backend: ocx_trust::key_ref::KeyBackendKind::File,
                public_key_hint: None,
            },
        )
    }

    /// A published sidecar holding `count` layers this client did not author.
    ///
    /// Each layer carries a distinct digest *and* a distinct signature
    /// annotation, so none of them can be mistaken for the layer under test by
    /// [`is_same_layer`] — the tests below all turn on "our layer is not in
    /// there", and a seeded collision would make them pass for the wrong
    /// reason.
    fn foreign_sidecar(count: usize) -> Vec<u8> {
        let layers: Vec<_> = (0..count)
            .map(|n| {
                serde_json::json!({
                    "mediaType": SIMPLESIGNING_MEDIA_TYPE,
                    "digest": format!("sha256:{:02x}{}", n, "ff".repeat(31)),
                    "size": 9,
                    "annotations": { ANNOTATION_COSIGN_SIGNATURE: format!("foreign-{n}") },
                })
            })
            .collect();
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_IMAGE_MEDIA_TYPE,
            "config": {
                "mediaType": EMPTY_CONFIG,
                "digest": EMPTY_CONFIG_DIGEST,
                "size": EMPTY_CONFIG_SIZE,
            },
            "layers": layers,
        }))
        .expect("foreign sidecar manifest")
    }

    /// Seed `bytes` at the `.sig` tag of the shared subject.
    fn seed(data: &StubTransportData, key: &str, bytes: Vec<u8>) {
        let digest = Algorithm::Sha256.hash(&bytes).to_string();
        data.write().manifests.insert(key.to_string(), (bytes, digest));
    }

    /// Every message in the error's cause chain, joined.
    ///
    /// `SignErrorKind::Internal` renders as the bare "internal signing error"
    /// and carries its cause behind `#[source]` — deliberately, so
    /// `classify_error` can chain-walk to the `ClientError` underneath. A test
    /// asserting on `to_string()` alone therefore sees the same eight words for
    /// a 503, a lost update and a layer-limit refusal, and cannot tell the
    /// three apart.
    fn causes(error: &SignErrorKind) -> String {
        let mut rendered = error.to_string();
        let mut current = std::error::Error::source(error);
        while let Some(source) = current {
            rendered.push_str(" | ");
            rendered.push_str(&source.to_string());
            current = source.source();
        }
        rendered
    }

    fn push_count(data: &StubTransportData) -> usize {
        data.read()
            .calls
            .iter()
            .filter(|call| call.as_str() == "push_manifest_raw")
            .count()
    }

    /// A sidecar read that fails for any reason but "not found" must NOT be
    /// treated as an empty tag.
    ///
    /// Failure this pins: `read_sidecar`'s `Err(other) => Err(map_client_error(other))`
    /// arm collapsed to `Ok(None)`. A 401, 429, 5xx or timeout on the
    /// `sha256-<hex>.sig` read then reads as "no sidecar exists", the append
    /// PUTs a manifest holding only our layer over every signature another
    /// signer published at that tag, and the read-back — seeing our own layer —
    /// reports success. The module doc names exactly this: "treating the second
    /// as the first republishes an empty manifest over every signature this
    /// client did not author."
    ///
    /// Asserted on three axes on purpose, because each alone is satisfiable by
    /// the broken code: the call fails (the mutant also fails, but with the
    /// *exhaustion* message), the registry fault reaches the caller rather than
    /// being relabelled, and — the one that cannot be faked — the foreign
    /// manifest is still the published one, because nothing was ever pushed.
    #[tokio::test]
    async fn a_sidecar_read_that_fails_is_never_read_as_an_absent_sidecar() {
        let data = StubTransportData::new();
        let (client, image, key) = client_and_sidecar_key(&data);
        let published = foreign_sidecar(1);
        seed(&data, &key, published.clone());
        {
            let mut inner = data.write();
            // Wins over the seeded manifest: a healthy, published sidecar whose
            // read fails transiently is the whole shape under test.
            inner
                .manifest_errors
                .insert(key.clone(), "503 Service Unavailable".to_string());
            // So that a PUT the mutant makes would be observable in the store.
            inner.capture_pushes = true;
        }

        let error = append_layer(client.transport(), &image, &subject(), &our_layer())
            .await
            .expect_err("a 503 on the sidecar read must not be reported as success");

        let rendered = causes(&error);
        assert!(
            rendered.contains("503 Service Unavailable"),
            "the registry fault must reach the caller, got: {rendered}"
        );
        assert!(
            !rendered.contains("concurrent writer"),
            "an unreadable sidecar is not a lost update, got: {rendered}"
        );
        assert_eq!(
            data.read().manifests.get(&key).map(|(bytes, _)| bytes.clone()),
            Some(published),
            "the signatures published at the sidecar tag must survive an unreadable read"
        );
        assert_eq!(
            push_count(&data),
            0,
            "nothing may be PUT to a sidecar tag whose current contents could not be read"
        );
    }

    /// Exhausting [`MAX_APPEND_ATTEMPTS`] is an error, never a silent success.
    ///
    /// Failure this pins: the terminal `Err(RegistryTransient)` collapsed to
    /// `Ok(digest)`. The caller is then told the signature landed when the
    /// read-back never saw it — against this module's documented contract,
    /// "Never an `Ok` that drops the signature."
    ///
    /// The rival wins every round here, not just the first: a stub that
    /// clobbers once converges on attempt 2 and can never reach the terminal
    /// arm at all. Modelled by a store that serves the rival's manifest and
    /// keeps it — every PUT is accepted and immediately overwritten, which is
    /// precisely the lost update a plain read-append-write cannot see.
    #[tokio::test]
    async fn a_sidecar_append_that_never_survives_is_reported_not_claimed() {
        let data = StubTransportData::new();
        let (client, image, key) = client_and_sidecar_key(&data);
        seed(&data, &key, foreign_sidecar(1));
        // `capture_pushes` left off: each PUT answers `Ok` and the tag keeps
        // serving the rival's document, so no attempt ever reads its own layer
        // back.

        let error = append_layer(client.transport(), &image, &subject(), &our_layer())
            .await
            .expect_err("a layer that never survives the read-back must not be reported as appended");

        let rendered = causes(&error);
        assert!(
            rendered.contains("concurrent writer") && rendered.contains("was not appended"),
            "the caller must be told the layer is missing, got: {rendered}"
        );
        // The literal as well as the constant: asserting only against
        // `MAX_APPEND_ATTEMPTS` moves with it, so cutting the budget to 1 —
        // which would report a lost update on the first ordinary race — stays
        // green. The number is the contract, not just the symbol.
        assert_eq!(
            push_count(&data),
            MAX_APPEND_ATTEMPTS,
            "the loop must spend its whole budget before giving up"
        );
        assert_eq!(MAX_APPEND_ATTEMPTS, 5, "the append budget is part of the contract");
    }

    /// A sidecar already at [`MAX_SIDECAR_LAYERS`] is refused before anything is
    /// written.
    ///
    /// Failure this pins: the bound deleted, or raised out of reach. A sidecar
    /// is a mutable tag anyone with push access authors and every layer is a
    /// signature a verifier will fetch and try, so unbounded growth is "a cheap
    /// way to make verification arbitrarily expensive for one subject" — this
    /// module's own words for why the bound exists.
    ///
    /// Seeded at exactly the limit rather than near it: no other test in the
    /// tree seeds more than two layers, so this is the only place the guard is
    /// reachable at all.
    #[tokio::test]
    async fn a_sidecar_at_the_layer_limit_refuses_a_further_append() {
        let data = StubTransportData::new();
        let (client, image, key) = client_and_sidecar_key(&data);
        seed(&data, &key, foreign_sidecar(MAX_SIDECAR_LAYERS));
        data.write().capture_pushes = true;

        let error = append_layer(client.transport(), &image, &subject(), &our_layer())
            .await
            .expect_err("appending past the layer limit must be refused");

        let rendered = causes(&error);
        assert!(
            rendered.contains(&format!("holds {MAX_SIDECAR_LAYERS} layers")),
            "the refusal must name the bound it enforces, got: {rendered}"
        );
        assert_eq!(
            push_count(&data),
            0,
            "the bound must be enforced before the manifest is rewritten, not after"
        );
        // Pinned as a literal for the same reason the append budget is: the
        // seed above follows the constant, so raising the bound would raise the
        // seed with it and leave this test green while verification got more
        // expensive per subject.
        assert_eq!(MAX_SIDECAR_LAYERS, 32, "the layer bound is part of the contract");
    }

    /// A document at the sidecar tag that is not an image manifest is refused,
    /// not appended to.
    ///
    /// The untrusted-input arm: a sidecar is a mutable tag anyone with push
    /// access authors, so the bytes at it are attacker-controlled and need not
    /// be a manifest at all. Parsing them is where that input enters, and the
    /// refusal must happen before the tag is rewritten — otherwise a single
    /// junk PUT by anyone would get laundered into a manifest signed by us.
    #[tokio::test]
    async fn a_sidecar_tag_holding_something_other_than_a_manifest_is_refused() {
        let data = StubTransportData::new();
        let (client, image, key) = client_and_sidecar_key(&data);
        seed(&data, &key, b"<!doctype html><title>login</title>".to_vec());
        data.write().capture_pushes = true;
        let transport_key = key.clone();

        let error = append_layer(client.transport(), &image, &subject(), &our_layer())
            .await
            .expect_err("a sidecar tag holding a non-manifest must be refused");

        assert!(
            causes(&error).contains("is not an image manifest"),
            "the refusal must name what it could not parse, got: {}",
            causes(&error)
        );
        assert_eq!(
            push_count(&data),
            0,
            "nothing may be PUT over a document this client could not parse"
        );
        assert_eq!(
            data.read()
                .manifests
                .get(&transport_key)
                .map(|(bytes, _)| bytes.clone()),
            Some(b"<!doctype html><title>login</title>".to_vec()),
            "the unparseable document must be left exactly as it was found"
        );
    }

    /// Re-attesting a tag whose `.att` layer carries no signature annotation
    /// **appends the annotated layer beside it**, at the same blob digest — and
    /// a second re-attest publishes nothing at all.
    ///
    /// That layer is one no cosign release can verify, and a re-attest is the
    /// only thing that can repair it. Treating it as already-present because the
    /// digests match returns `Ok` with a `sidecar_digest`, publishes nothing,
    /// and leaves the tag exactly as unverifiable as it was — success reported
    /// for a repair that did not happen.
    ///
    /// The digests *do* match, which is what makes this reachable rather than
    /// theoretical: `PredicateType::wrap` adds no timestamp outside
    /// `URI_CUSTOM`, and p256's ECDSA is RFC 6979, so re-attesting one subject
    /// under one key reproduces the envelope byte-for-byte.
    ///
    /// **Asserted as the whole layer list, not as an existential over it.**
    /// `any(annotation.is_some())` is equally true of a [`rebuild_with`] that
    /// *replaced* the published layers — the one thing this module's doc
    /// forbids, because "replacing would silently delete a signature someone
    /// else published". The shape pins both halves: the unannotated layer
    /// someone else pushed is still there, and the annotated one is beside it.
    ///
    /// **Appended twice on purpose.** A repair is only a repair if it
    /// converges: the second call reads back the layer the first one published,
    /// `Some("") == Some("")`, and pushes nothing. Without that assertion a
    /// re-attest would be free to burn one of the [`MAX_SIDECAR_LAYERS`] slots
    /// every run — the only bound on this tag — with the whole suite green.
    #[tokio::test]
    async fn an_att_repair_appends_beside_the_broken_layer_and_then_converges() {
        let envelope = b"{\"payloadType\":\"application/vnd.in-toto+json\"}".to_vec();
        let digest = Algorithm::Sha256.hash(&envelope).to_string();
        let data = StubTransportData::new();
        let (client, image, _) = client_and_sidecar_key(&data);
        let key = sibling_tag_reference(&image, sidecar_tag(&subject(), SidecarKind::Attestation)).to_string();
        let published = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_IMAGE_MEDIA_TYPE,
            "config": {
                "mediaType": EMPTY_CONFIG,
                "digest": EMPTY_CONFIG_DIGEST,
                "size": EMPTY_CONFIG_SIZE,
            },
            "layers": [
                // Someone else's signature, with its verification material. Not
                // the layer under repair — it is here so the shape assertion
                // below can see an annotation the append had to carry over,
                // which a re-emit that dropped `annotations` would lose while
                // still leaving the layer *count* right.
                {
                    "mediaType": SIMPLESIGNING_MEDIA_TYPE,
                    "digest": format!("sha256:{}", "ab".repeat(32)),
                    "size": 9,
                    "annotations": { ANNOTATION_COSIGN_SIGNATURE: "foreign" },
                },
                {
                    "mediaType": DSSE_ENVELOPE_MEDIA_TYPE,
                    "digest": digest,
                    "size": envelope.len(),
                },
            ],
        }))
        .expect("an unannotated attestation sidecar");
        seed(&data, &key, published);
        data.write().capture_pushes = true;

        let layer = SidecarLayer::attestation(envelope, None, None);
        append_layer(client.transport(), &image, &subject(), &layer)
            .await
            .expect("re-attesting an unverifiable sidecar must repair it");
        append_layer(client.transport(), &image, &subject(), &layer)
            .await
            .expect("a repaired sidecar must re-attest without publishing again");

        assert_eq!(
            push_count(&data),
            1,
            "the annotated layer must reach the registry exactly once: deduping on the digest \
             alone leaves the tag unverifiable by every cosign release, and failing to dedupe \
             against the layer just repaired spends one of the {MAX_SIDECAR_LAYERS} slots per \
             re-attest"
        );
        let (bytes, _) = data.read().manifests.get(&key).cloned().expect("the tag was rewritten");
        let manifest: ImageManifest = serde_json::from_slice(&bytes).expect("the rewritten sidecar parses");
        let shape: Vec<_> = manifest
            .layers
            .iter()
            .map(|held| (held.digest.as_str(), signature_annotation(held).map(String::as_str)))
            .collect();
        assert_eq!(
            shape,
            [
                (format!("sha256:{}", "ab".repeat(32)).as_str(), Some("foreign")),
                (digest.as_str(), None),
                (digest.as_str(), Some("")),
            ],
            "the annotated layer must be appended BESIDE what someone else published — every \
             held layer, and every annotation on it, survives the rewrite"
        );
    }

    /// cosign's `.att` reader keys on the *presence* of the signature
    /// annotation, so an attestation layer must carry it — empty.
    ///
    /// Failure this pins: the annotation omitted, as it was until this branch.
    /// `cosign verify-attestation` then refuses every `.att` sidecar OCX
    /// publishes ("signature layer sha256:… is missing
    /// dev.cosignproject.cosign/signature annotation"), which no OCX-side test
    /// could see. The empty value is cosign's own: it is what
    /// `attach attestation` writes, pinned by
    /// `test/tests/fixtures/golden/attestation_sidecar_key_manifest.json`.
    #[test]
    fn an_attestation_layer_carries_cosigns_empty_signature_annotation() {
        let layer = SidecarLayer::attestation(b"{\"payloadType\":\"x\"}".to_vec(), None, None).descriptor();
        assert_eq!(
            signature_annotation(&layer).map(String::as_str),
            Some(""),
            "cosign refuses an .att layer with no signature annotation"
        );
    }
}
