// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Writing a cosign sidecar tag — `sha256-<hex>.sig` and `sha256-<hex>.att`.
//!
//! One append loop, two layer shapes. A `.sig` layer is a simplesigning claim
//! with a **detached** signature in its annotations; an `.att` layer is a DSSE
//! envelope carrying its signature **inside**. [`SidecarLayer`] is where that
//! difference lives, and its two constructors are the only way to build one, so
//! a tag suffix can never be paired with the wrong media type.
//!
//! Not a re-packaging of the v0.3 bundle: a simplesigning signature covers a
//! *different payload*. The claim
//! (`{"critical":{"identity":…,"image":…,"type":…},"optional":…}`) is signed as
//! opaque bytes, its Rekor entry is a `hashedrekord` over those bytes, and the
//! verification material travels in **layer annotations** rather than in a
//! bundle blob. Under `--signature-format both` that costs a second Fulcio
//! certificate and a second Rekor entry per subject, which is the honest price
//! of emitting two independent signatures.
//!
//! # Why re-signing appends
//!
//! The sidecar is one image manifest whose layers are the signatures. A second
//! signature over the same subject — a second signer, a re-sign after a key
//! rotation — is a second layer, not a replacement: replacing would silently
//! delete a signature someone else published. That makes every write a
//! read-modify-write against a mutable tag, with the same lost-update hazard
//! decision D4 names for the referrers fallback index, and the same remedy:
//! read, append, push, **read back**, retry.
//!
//! The read-back is the whole mechanism. A PUT that a concurrent writer
//! immediately clobbered returns `Ok` and loses the layer; only re-reading
//! catches it.

use std::collections::BTreeMap;

use serde::Serialize;

use super::error::SignErrorKind;
use super::referrers::map_client_error;
use super::rekor::RekorEntry;
use super::signer::SignedBlob;
use crate::oci::client::OciTransport;
use crate::oci::client::error::ClientError;
use crate::oci::referrer::media_types::{
    ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE, ANNOTATION_COSIGN_CHAIN, ANNOTATION_COSIGN_SIGNATURE,
    DSSE_ENVELOPE_MEDIA_TYPE, EMPTY_CONFIG, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_PAYLOAD, EMPTY_CONFIG_SIZE,
    SIMPLESIGNING_MEDIA_TYPE,
};
use crate::oci::verify::SidecarKind;
use crate::oci::{Algorithm, Descriptor, Digest, ImageManifest, OCI_IMAGE_MEDIA_TYPE, native};

/// Attempts before a concurrent writer is reported rather than silently losing
/// the signature.
///
/// The same budget the referrers fallback index uses, for the same reason: two
/// writers converge provably (the loser re-reads and sees the winner), three do
/// not, so the bound is what turns an unbounded race into a loud, retryable
/// failure.
const MAX_APPEND_ATTEMPTS: usize = 5;

/// Layers one sidecar manifest may carry.
///
/// A sidecar is a mutable tag anyone with push access authors, and every layer
/// is a signature a verifier will fetch and try. Unbounded, it is a cheap way to
/// make verification arbitrarily expensive for one subject.
const MAX_SIDECAR_LAYERS: usize = 32;

/// One sidecar layer, ready to append: which tag it belongs under, what types
/// it, the bytes it holds, and the verification material a reader needs.
///
/// Grouped rather than passed as four positionals because the first two are one
/// decision — a `.sig` tag carries a simplesigning claim, an `.att` tag carries
/// a DSSE envelope — and as adjacent arguments a swapped pair would type-check
/// and publish a layer no cosign reader accepts. The two constructors below are
/// the only way to build one.
pub(crate) struct SidecarLayer {
    /// Which sidecar tag this layer belongs under.
    ///
    /// The reader's own enum, not a suffix string of this module's: the tag is
    /// spelled once, in [`crate::oci::verify::sidecar_tag`], and the writer
    /// that formatted it by hand here disagreed with the reader about the
    /// truncated-digest half for every non-sha256 subject.
    kind: SidecarKind,
    /// The layer descriptor's `mediaType`.
    media_type: &'static str,
    /// The layer blob. Its SHA-256 is the layer's registry address.
    payload: Vec<u8>,
    /// Verification material, in cosign's annotation vocabulary.
    annotations: BTreeMap<String, String>,
}

impl SidecarLayer {
    /// The `sha256-<hex>.sig` layer for one simplesigning claim.
    ///
    /// `payload` is the exact claim bytes the signature covers.
    pub(crate) fn signature(payload: Vec<u8>, signed: &SignedBlob) -> Self {
        use base64::Engine as _;
        let base64 = base64::engine::general_purpose::STANDARD;

        let mut annotations = BTreeMap::new();
        // The one annotation only this shape carries: a simplesigning signature
        // is DETACHED from the payload it covers, so the bytes alone are not
        // the signature and the annotation is where a verifier finds it.
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

    /// The `sha256-<hex>.att` layer for one DSSE-enveloped attestation.
    ///
    /// `envelope` is the DSSE envelope's own JSON — the same document the
    /// Sigstore bundle wraps, published bare here because that is the shape
    /// cosign's `.att` tag has always held.
    ///
    /// **`dev.cosignproject.cosign/signature` is present and empty.** On a
    /// `.sig` layer that key carries a signature *detached* from the payload;
    /// a DSSE envelope carries its signatures inside, in `signatures[].sig`,
    /// so there is nothing for the value to hold. The key is still written,
    /// because on `.att` cosign reads it as a **presence marker** rather than
    /// as material: `cosign verify-attestation` refuses a layer without it
    /// ("signature layer sha256:… is missing dev.cosignproject.cosign/signature
    /// annotation") and cosign's own `attach attestation` writes it empty —
    /// pinned by the golden capture
    /// `test/tests/fixtures/golden/attestation_sidecar_key_manifest.json`.
    /// Omitting it, as this constructor used to, published `.att` sidecars no
    /// cosign release can verify.
    ///
    /// The empty value is therefore not "material that is not there": it is the
    /// whole of what cosign writes in this position. Everything genuinely
    /// optional — certificate, bundle — is still omitted when absent.
    pub(crate) fn attestation(envelope: Vec<u8>, certificate_pem: Option<&str>, rekor_bundle: Option<&str>) -> Self {
        let mut annotations = BTreeMap::new();
        annotations.insert(ANNOTATION_COSIGN_SIGNATURE.to_string(), String::new());
        // No chain: bundle v0.3 replaced the chain field with a single leaf and
        // Fulcio's intermediates come from the trust root, so there is nothing
        // a `/chain` annotation could carry that the signer produced.
        insert_present(&mut annotations, certificate_pem, None, rekor_bundle);
        Self {
            kind: SidecarKind::Attestation,
            media_type: DSSE_ENVELOPE_MEDIA_TYPE,
            payload: envelope,
            annotations,
        }
    }

    /// The descriptor this layer occupies in the sidecar manifest.
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

/// Insert the verification material cosign reads out of layer annotations,
/// omitting whatever the signer did not produce.
///
/// Absent material is **omitted**, never written empty: under a key there is no
/// certificate, and with no transparency-log upload there is no bundle. An empty
/// annotation would look like present-but-broken material to a reader that only
/// checks for the key.
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

/// Append `layer` to the sidecar manifest at its tag, creating the manifest
/// when absent.
///
/// `image` **must** be the write reference (`Client::transport_write_reference`).
/// This PUTs a tag through whatever host it is handed, and a signature written
/// to a read-only mirror is one the canonical verifier never looks at — the
/// CWE-345/367 class `oci/client.rs` documents at its addressing seams.
///
/// # Errors
///
/// - Whatever the blob or manifest push raises.
/// - [`SignErrorKind::Internal`] wrapping [`ClientError::RegistryTransient`]
///   when concurrent writers exhaust [`MAX_APPEND_ATTEMPTS`]. Never an `Ok`
///   that drops the signature.
/// - [`SignErrorKind::Internal`] wrapping [`ClientError::InvalidManifest`] when
///   the sidecar already holds [`MAX_SIDECAR_LAYERS`] layers.
pub(crate) async fn append_layer(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject: &Digest,
    layer: &SidecarLayer,
) -> Result<Digest, SignErrorKind> {
    let payload = layer.payload.as_slice();
    let tag = crate::oci::verify::sidecar_tag(subject, layer.kind);
    // The one seam that only changes the tag: registry and repository come
    // from a reference a seam already resolved, so no host is minted here.
    let target = crate::oci::client::sibling_tag_reference(image, tag.clone());
    let layer = layer.descriptor();

    // The blob is content-addressed and identical across attempts, so it is
    // pushed once outside the loop; only the manifest is contended.
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
            // Idempotent, so a retry after an ambiguous read-back does not
            // stack duplicates. The digest reported is the one the registry
            // served, never a re-serialization of the parsed manifest: serde
            // may not reproduce the published bytes, and a digest that names no
            // manifest at the tag is worse than no digest at all.
            return Ok(served_digest.clone());
        }
        if existing.layers.len() >= MAX_SIDECAR_LAYERS {
            crate::log::warn!(
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

        // Constructed, never echoed: the manifest at the tag is authored by
        // anyone with push access and this call re-publishes it under the
        // caller's own credentials, so its header is rebuilt and its layers are
        // re-emitted field by field from values that parsed.
        let next = rebuild_with(existing, layer.clone());
        let bytes = serde_json::to_vec(&next).map_err(serialization)?;
        let digest = Algorithm::Sha256.hash(&bytes);
        transport
            .push_manifest_raw(&target, bytes, OCI_IMAGE_MEDIA_TYPE)
            .await
            .map_err(map_client_error)?;

        // The only evidence the PUT survived: a concurrent writer that read the
        // same base manifest and pushed after us produced an `Ok` above while
        // dropping this layer.
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

/// Whether `held` is the layer `layer` carries, rather than merely a layer with
/// the same address.
///
/// **On the `.sig` shape the digest alone is not identity.**
/// `SimpleSigningClaim::new` writes no `optional` section, so the claim bytes
/// are a pure function of the repository and the subject digest: a second
/// signer, or a re-sign after a key rotation, produces a **byte-identical
/// payload under a different signature**. Deduping on the digest would return
/// `Ok` while dropping that second signature, and — worse — the read-back would
/// accept a concurrent writer's clobber as proof that our own layer survived,
/// which is the one thing it exists to catch. The detached-signature annotation
/// is what tells the two apart.
///
/// **On the `.att` shape the annotation is a constant, not a discriminator** —
/// among the layers this client writes. It is cosign's presence marker and is
/// always empty there (see [`SidecarLayer::attestation`]), so between two of our
/// own `.att` layers the digest is what decides. One rule, because the rule is
/// "compare whatever is detached from the payload", and on `.att` nothing is.
///
/// **An unannotated layer is not the same layer**, even at the same digest. A
/// `.att` layer carrying no `dev.cosignproject.cosign/signature` key is one no
/// cosign release can verify, so a re-attest must publish the annotated layer
/// rather than dedupe against the broken one and report success for a tag it
/// leaves unverifiable. `None != Some("")` is what makes that append happen.
///
/// Comparing the *whole* annotation map instead would be strictly wrong: the
/// verification material is optional (a key-mode signature has no certificate),
/// so a re-append that carried less of it than the published layer would read as
/// a different signature and stack a duplicate.
///
/// A re-sign is **not** guaranteed to produce a new signature: p256's ECDSA is
/// RFC 6979, so the file backend is deterministic (`key_backend.rs`) and
/// re-signing one payload under one key yields the same bytes. That makes a
/// repeat of the identical signature dedupe to `Ok` without appending, which is
/// right — it would add a byte-identical layer. What the annotation catches is
/// the case that matters: a *different* signer, or the same subject after a key
/// rotation, whose signature differs over the very same claim bytes.
fn is_same_layer(held: &Descriptor, layer: &Descriptor) -> bool {
    held.digest == layer.digest && signature_annotation(held) == signature_annotation(layer)
}

/// The `dev.cosignproject.cosign/signature` annotation, when the descriptor
/// carries one.
///
/// `Some` with a real base64 signature on a `.sig` layer this client wrote;
/// `Some("")` on an `.att` layer, where the key is cosign's presence marker;
/// `None` on anything else someone pushed to the tag — and no cosign release can
/// verify *that layer*, which is why [`is_same_layer`] treats it as a different
/// layer rather than a match, appending the annotated one beside it.
fn signature_annotation(descriptor: &Descriptor) -> Option<&String> {
    descriptor.annotations.as_ref()?.get(ANNOTATION_COSIGN_SIGNATURE)
}

/// Read the sidecar manifest at `target`, and the digest the registry served it
/// under. `None` when the tag does not exist.
///
/// An absent tag is the first signature's starting point, and is the one refusal
/// reported as `Ok(None)`. Every other refusal is an error, because a caller
/// that appends must be able to tell "there is nothing there" from "I could not
/// read what is there" — treating the second as the first republishes an empty
/// manifest over every signature this client did not author.
async fn read_sidecar(
    transport: &dyn OciTransport,
    target: &native::Reference,
) -> Result<Option<(ImageManifest, Digest)>, SignErrorKind> {
    match transport
        .pull_manifest_raw(target, crate::media_type::ACCEPTED_MANIFEST_MEDIA_TYPES)
        .await
    {
        Ok((bytes, _served_digest)) => {
            let manifest: ImageManifest = serde_json::from_slice(&bytes).map_err(|error| {
                SignErrorKind::Internal(Box::new(ClientError::InvalidManifest(format!(
                    "simplesigning sidecar is not an image manifest: {error}"
                ))))
            })?;
            // Hashed from the bytes the registry served, not re-serialized from
            // the parsed manifest: serde need not reproduce the published byte
            // order, and a digest naming no manifest at the tag is worse than
            // none. Recomputed rather than taken from the response header, so
            // the value is the content's own address either way.
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
/// The config descriptor is carried over rather than reset. cosign's own `.sig`
/// manifests point at a real image config (233 bytes in the committed golden
/// capture), and replacing it with the OCI empty config would rewrite — and
/// orphan — what cosign published, for an append that has no business touching
/// it. Carried field by field like the layers, so nothing echoes a struct that
/// merely parsed.
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

/// The `dev.sigstore.cosign/bundle` annotation value for `entry`.
///
/// cosign's offline transparency-log material: the SET plus the log entry it
/// covers, so a `.sig` verifies without contacting Rekor. Field names and
/// capitalisation are Go struct tags on cosign's side and are wire, not style.
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

/// Wrap a serialization failure, which is a bug rather than a registry fault.
fn serialization(error: serde_json::Error) -> SignErrorKind {
    SignErrorKind::Internal(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Identifier;
    use crate::oci::client::Client;
    use crate::oci::client::sibling_tag_reference;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::verify::sidecar_tag;

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
        let image = client
            .transport_write_reference(&Identifier::parse("registry.example/team/pkg:1.0").expect("test identifier"));
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
                key_backend: crate::oci::sign::KeyBackendKind::File,
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
