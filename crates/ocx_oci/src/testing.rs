// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one test-support surface this crate lends its consumers (plan C-053).
//!
//! `OciTransport` is the seam every registry read and write crosses, and the
//! SSRF guard, the retry ladder and the mirror routing all hang off the
//! production implementation of it. A consumer that wants to drive a pipeline
//! without a registry therefore needs a transport double — and before the
//! split each tier simply wrote one, because the trait and the doubles lived in
//! the same crate.
//!
//! Across a crate boundary that would mean either widening the production
//! seams permanently or letting every consumer implement the transport trait
//! itself. This module is the third answer: the doubles and the seams that
//! construct a [`Client`](crate::Client) around one are `pub` **only** under
//! `cfg(test)` or the `__testing` feature, so a release build physically lacks
//! them, and consumers name one module rather than reaching into
//! `client::test_transport`.
//!
//! Enable it from a `[dev-dependencies]` row:
//!
//! ```toml
//! ocx_oci = { workspace = true, features = ["__testing"] }
//! ```
//!
//! Resolver v3 keeps a dev-only feature out of the normal build, so the runtime
//! dependency row stays seamless.

pub use crate::client::push_blob_buffered;
pub use crate::client::test_transport::{StubTransport, StubTransportData, referrers_key};

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::client::error::ClientError;
use crate::client::{OciTransport, ProgressFn};
use crate::referrer::ReferrerManifest;
use crate::{Algorithm, Descriptor, Digest, ImageManifest, Manifest, Reference, RegistryOperation};

/// What every method of a double in this module answers with.
type Answer<T> = std::result::Result<T, ClientError>;

// ── RecordingTransport ───────────────────────────────────────────────────────

/// The transport double the sign, attest and verify pipelines drive their unit
/// tests through: it records `"<method>:<registry>"` for every call, so a test
/// can assert which host the pipeline actually talked to, and it holds a
/// tag-addressed manifest store, so the referrers fallback index's
/// read-append-write-read-back loop runs against a registry that really keeps
/// what it was handed.
///
/// One type rather than three, because the three pipelines wanted the same
/// registry with different contents. Everything a pipeline varies is a field
/// set through the builder below, and every field is inert at its default —
/// [`Default`] alone is the sign/attest shape minus its subject digest, which
/// is the one thing with no sensible default and so is asserted on first use.
#[derive(Clone, Default)]
pub struct RecordingTransport {
    calls: Arc<Mutex<Vec<String>>>,
    /// Tag- and digest-addressed manifest store, keyed by whole reference.
    manifests: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Every referrer manifest pushed, verbatim — the bytes a cosign reader
    /// would see, so a test can assert on annotations rather than on the exit
    /// code.
    referrer_manifests: Arc<Mutex<Vec<Vec<u8>>>>,
    /// The digest a manifest read that misses the store claims. Set it to
    /// something other than the resolved subject digest to model a mirror
    /// serving the wrong manifest.
    subject_digest: Option<String>,
    /// Bytes served for a read addressed *at* [`Self::subject_digest`]. `None`
    /// falls through to [`Self::default_manifest`], which is what a pipeline
    /// that never re-hashes the subject wants.
    subject_manifest: Option<Vec<u8>>,
    /// Bytes served for any other read that misses the store; `None` is `{}`.
    default_manifest: Option<Vec<u8>>,
    /// Answer a `sha256-`-prefixed tag read with `ManifestNotFound`, the way a
    /// registry with no fallback index yet does. Off by default, because a
    /// pipeline that *serves* a referrer at such a tag must not 404 it.
    sidecar_tags_absent: bool,
    /// What a referrers listing returns; empty is the successful listing the
    /// capability probe reads as "this registry has the Referrers API".
    referrers: Vec<Descriptor>,
    /// Answer `list_referrers` with `ReferrersUnsupported` — the `registry:2`
    /// shape, where the tag-schema fallback is the only way a referrer becomes
    /// discoverable.
    referrers_unsupported: bool,
    /// Bytes every blob stream yields.
    blob_stream: Option<Vec<u8>>,
    /// When set, the FIRST manifest PUT to a `.sig` tag is answered `Ok` and
    /// then overwritten by these bytes — the lost update a plain
    /// read-append-write cannot see, and the reason the append reads back.
    clobber_first_sidecar_put: Option<Vec<u8>>,
    /// Whether that clobber has already fired.
    clobbered: Arc<Mutex<bool>>,
    /// Panic on any write. A read-only pipeline says so here, so a push it was
    /// never meant to make fails loudly instead of quietly succeeding.
    refuse_pushes: bool,
}

impl RecordingTransport {
    /// The digest a manifest read that misses the store claims.
    #[must_use]
    pub fn serving_subject_digest(mut self, digest: impl Into<String>) -> Self {
        self.subject_digest = Some(digest.into());
        self
    }

    /// Serve `bytes` for a read addressed at the subject digest — the preimage
    /// a pipeline that re-hashes the subject manifest needs.
    #[must_use]
    pub fn serving_subject_manifest(mut self, bytes: &[u8]) -> Self {
        self.subject_manifest = Some(bytes.to_vec());
        self
    }

    /// Serve `bytes` for every manifest read that misses the store and is not
    /// the subject.
    #[must_use]
    pub fn serving_manifest(mut self, bytes: &[u8]) -> Self {
        self.default_manifest = Some(bytes.to_vec());
        self
    }

    /// 404 a `sha256-`-prefixed tag read: no fallback index, no cosign sidecar.
    #[must_use]
    pub fn absent_sidecar_tags(mut self) -> Self {
        self.sidecar_tags_absent = true;
        self
    }

    /// Answer every referrers listing with `ReferrersUnsupported`.
    #[must_use]
    pub fn referrers_unsupported(mut self) -> Self {
        self.referrers_unsupported = true;
        self
    }

    /// Answer every referrers listing with `descriptors`.
    #[must_use]
    pub fn serving_referrers(mut self, descriptors: Vec<Descriptor>) -> Self {
        self.referrers = descriptors;
        self
    }

    /// Yield `bytes` from every blob stream.
    #[must_use]
    pub fn serving_blob(mut self, bytes: &[u8]) -> Self {
        self.blob_stream = Some(bytes.to_vec());
        self
    }

    /// Let a rival writer clobber the first `.sig` sidecar PUT with `rival`.
    #[must_use]
    pub fn clobbering_first_sidecar_put(mut self, rival: &[u8]) -> Self {
        self.clobber_first_sidecar_put = Some(rival.to_vec());
        self
    }

    /// Panic on any write, for a pipeline that only reads.
    #[must_use]
    pub fn refusing_pushes(mut self) -> Self {
        self.refuse_pushes = true;
        self
    }

    /// The `"<method>:<registry>"` log, in call order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("recorder lock").clone()
    }

    /// The bytes stored at `reference`, if any — how a test reads back the
    /// fallback index this transport was asked to hold.
    pub fn stored(&self, reference: &str) -> Option<Vec<u8>> {
        self.manifests
            .lock()
            .expect("manifest store lock")
            .get(reference)
            .cloned()
    }

    /// Place `bytes` at `reference` before the run, standing in for a document
    /// some other client published.
    pub fn seed(&self, reference: &str, bytes: Vec<u8>) {
        self.manifests
            .lock()
            .expect("manifest store lock")
            .insert(reference.to_string(), bytes);
    }

    /// Every referrer manifest pushed, verbatim, in push order.
    pub fn referrer_manifests(&self) -> Vec<Vec<u8>> {
        self.referrer_manifests.lock().expect("recorder lock").clone()
    }

    /// The one referrer manifest a successful run pushes, as JSON.
    ///
    /// # Panics
    ///
    /// If the run pushed anything other than exactly one — a count a caller
    /// asserting on the body always means to pin.
    pub fn pushed_referrer(&self) -> serde_json::Value {
        let pushed = self.referrer_manifests();
        let [bytes] = pushed.as_slice() else {
            panic!("expected exactly one referrer manifest push, got {}", pushed.len());
        };
        serde_json::from_slice(bytes).expect("referrer manifest is JSON")
    }

    fn record(&self, method: &str, image: &Reference) {
        self.calls
            .lock()
            .expect("recorder lock")
            .push(format!("{method}:{}", image.resolve_registry()));
    }

    fn claimed_subject_digest(&self) -> String {
        self.subject_digest
            .clone()
            .expect("a RecordingTransport serving manifests needs `serving_subject_digest`")
    }

    fn refuse_write(&self, method: &str) {
        assert!(
            !self.refuse_pushes,
            "a RecordingTransport built with `refusing_pushes` was asked to {method}"
        );
    }
}

#[async_trait]
impl OciTransport for RecordingTransport {
    async fn ensure_auth(&self, image: &Reference, _: RegistryOperation) -> Answer<()> {
        self.record("ensure_auth", image);
        Ok(())
    }

    async fn list_tags(&self, _: &Reference, _: usize, _: Option<String>) -> Answer<Vec<String>> {
        unimplemented!("a RecordingTransport never lists tags")
    }

    async fn catalog(&self, _: &Reference, _: usize, _: Option<String>) -> Answer<Vec<String>> {
        unimplemented!("a RecordingTransport never reads the catalog")
    }

    async fn fetch_manifest_digest(&self, _: &Reference) -> Answer<String> {
        unimplemented!("a RecordingTransport resolves digests through the index")
    }

    async fn pull_manifest_raw(&self, image: &Reference, _: &[&str]) -> Answer<(Vec<u8>, String)> {
        self.record("pull_manifest_raw", image);
        // The fallback index is tag-addressed, so its read must be served from
        // the store this transport writes into; anything else is a manifest the
        // fixture describes.
        if let Some(bytes) = self.stored(&image.whole()) {
            let digest = Algorithm::Sha256.hash(&bytes).to_string();
            return Ok((bytes, digest));
        }
        let claimed = self.claimed_subject_digest();
        if image.digest() == Some(claimed.as_str())
            && let Some(bytes) = self.subject_manifest.clone()
        {
            return Ok((bytes, claimed));
        }
        if self.sidecar_tags_absent && image.tag().is_some_and(|tag| tag.starts_with("sha256-")) {
            return Err(ClientError::ManifestNotFound(image.whole()));
        }
        Ok((self.default_manifest.clone().unwrap_or_else(|| b"{}".to_vec()), claimed))
    }

    async fn pull_blob(&self, _: &Reference, _: &Digest) -> Answer<Vec<u8>> {
        unimplemented!("a RecordingTransport streams blobs")
    }

    async fn pull_blob_streaming(
        &self,
        image: &Reference,
        _: &Digest,
    ) -> Answer<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>> {
        self.record("pull_blob_streaming", image);
        let body = self
            .blob_stream
            .clone()
            .expect("a RecordingTransport asked for a blob stream needs `serving_blob`");
        Ok(Box::new(std::io::Cursor::new(body)))
    }

    async fn pull_blob_to_file(&self, _: &Reference, _: &Digest, _: &Path) -> Answer<()> {
        unimplemented!("a RecordingTransport never writes blobs to disk")
    }

    async fn head_blob(&self, _: &Reference, _: &Digest) -> Answer<u64> {
        unimplemented!("a RecordingTransport never HEADs blobs")
    }

    async fn push_manifest(&self, _: &Reference, _: &Manifest) -> Answer<String> {
        unimplemented!("a RecordingTransport is written to through push_manifest_raw")
    }

    async fn push_manifest_raw(&self, image: &Reference, bytes: Vec<u8>, _: &str) -> Answer<String> {
        self.refuse_write("push_manifest_raw");
        // Records the whole reference, not just the registry: the fallback
        // index is identified by its TAG, and a recorder that drops the tag
        // cannot tell a fallback write from any other manifest PUT.
        let key = image.whole();
        self.calls
            .lock()
            .expect("recorder lock")
            .push(format!("push_manifest_raw:{key}"));
        // A concurrent writer read the same base manifest and pushed after us:
        // our PUT returned `Ok` and its layer is gone. The guards are ordered so
        // the "already fired" flag is only spent on a PUT that would clobber.
        let rival = match &self.clobber_first_sidecar_put {
            Some(rival)
                if key.ends_with(".sig")
                    && !std::mem::replace(&mut *self.clobbered.lock().expect("clobber lock"), true) =>
            {
                Some(rival.clone())
            }
            _ => None,
        };
        self.manifests
            .lock()
            .expect("manifest store lock")
            .insert(key.clone(), rival.unwrap_or(bytes));
        Ok(key)
    }

    async fn push_blob(&self, image: &Reference, _: Vec<u8>, digest: &Digest, _: ProgressFn) -> Answer<String> {
        self.refuse_write("push_blob");
        self.record("push_blob", image);
        Ok(digest.to_string())
    }

    async fn push_blob_from_path(
        &self,
        image: &Reference,
        path: &Path,
        digest: &Digest,
        on_progress: ProgressFn,
    ) -> Answer<String> {
        self.refuse_write("push_blob_from_path");
        push_blob_buffered(self, image, path, digest, on_progress).await
    }

    async fn push_referrer_manifest(
        &self,
        image: &Reference,
        _: &Digest,
        manifest_bytes: &[u8],
        media_type: &str,
    ) -> Answer<Descriptor> {
        self.refuse_write("push_referrer_manifest");
        self.record("push_referrer_manifest", image);
        self.referrer_manifests
            .lock()
            .expect("recorder lock")
            .push(manifest_bytes.to_vec());
        // `artifactType` and the annotations are read back out of the bytes just
        // pushed, exactly as `NativeTransport` does. A double that left them
        // `None` would pass a test asserting they survive the fallback append
        // while the real transport's value was never exercised.
        let manifest: ReferrerManifest =
            serde_json::from_slice(manifest_bytes).expect("the pipeline pushes a referrer manifest");
        Ok(Descriptor {
            media_type: media_type.to_string(),
            digest: Algorithm::Sha256.hash(manifest_bytes).to_string(),
            size: manifest_bytes.len() as i64,
            artifact_type: Some(manifest.artifact_type),
            annotations: manifest.annotations.map(|map| map.into_iter().collect()),
            ..Descriptor::default()
        })
    }

    async fn list_referrers(&self, image: &Reference, _: &Digest, _: Option<&str>) -> Answer<Vec<Descriptor>> {
        // A successful (empty) listing is what the capability probe reads as
        // "this registry supports the Referrers API"; the refusal is what it
        // reads as `Unsupported`.
        self.record("list_referrers", image);
        if self.referrers_unsupported {
            return Err(ClientError::ReferrersUnsupported {
                registry: image.resolve_registry().to_string(),
            });
        }
        Ok(self.referrers.clone())
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}

impl crate::sealed::Sealed for RecordingTransport {}

// ── SbomTransport ────────────────────────────────────────────────────────────

/// One referrer an [`SbomTransport`] serves: how it appears in a listing, the
/// manifest bytes behind that descriptor, and the blobs its layers name.
///
/// Plain data rather than a fixture builder: what a referrer *means* — an SBOM,
/// a mislabelled SBOM, an attestation bundle — is the consumer's vocabulary,
/// and this crate has no business knowing it.
#[derive(Clone)]
pub struct ServedReferrer {
    descriptor: Descriptor,
    manifest: Vec<u8>,
    blobs: Vec<Vec<u8>>,
}

impl ServedReferrer {
    /// `blobs` are the bodies this referrer's layers name, matched by digest.
    pub fn new(descriptor: Descriptor, manifest: Vec<u8>, blobs: Vec<Vec<u8>>) -> Self {
        Self {
            descriptor,
            manifest,
            blobs,
        }
    }

    fn blob_for(&self, digest: &Digest) -> Option<Vec<u8>> {
        self.blobs
            .iter()
            .find(|body| &Algorithm::Sha256.hash(body) == digest)
            .cloned()
    }
}

/// What a `sha256-<hex>.sbom` tag serves: one manifest and the blob each of its
/// layers names.
#[derive(Clone)]
struct SbomSidecar {
    manifest: Vec<u8>,
    /// One document per manifest layer, in manifest order.
    documents: Vec<Vec<u8>>,
}

impl SbomSidecar {
    fn digest(&self) -> Digest {
        Algorithm::Sha256.hash(&self.manifest)
    }

    /// The document the manifest's layers name under `digest`, matched by
    /// position: the digest is read out of the manifest rather than recomputed
    /// from the body, so a fixture whose layer digest does not cover its
    /// document is served exactly as inconsistently as a registry would serve
    /// it, instead of being silently repaired here.
    fn document_for(&self, digest: &str) -> Option<Vec<u8>> {
        let manifest: ImageManifest = serde_json::from_slice(&self.manifest).expect("sidecar manifest parses");
        let position = manifest.layers.iter().position(|layer| layer.digest == digest)?;
        Some(
            self.documents
                .get(position)
                .expect("the fixture serves one document per layer")
                .clone(),
        )
    }
}

/// Serves a caller-chosen referrer set — and, when one is hung on it, the
/// `sha256-<hex>.sbom` sidecar tag — recording what was asked for.
///
/// Honours the server-side `artifactType` filter, unlike the spec-permitted
/// registry that ignores it — which is the point: a signed pass must ask for
/// bundles and get only bundles, so an unsigned referrer reaching a
/// verification candidate would be this double's fault to expose and not to
/// hide.
#[derive(Clone)]
pub struct SbomTransport {
    /// The subject manifest, served under its own digest; everything else this
    /// transport holds hangs off it.
    subject_manifest: Vec<u8>,
    referrers: Vec<ServedReferrer>,
    /// `None` — the overwhelmingly common case — is a subject with no `.sbom`
    /// tag, which the transport answers with `ManifestNotFound`.
    sidecar: Option<SbomSidecar>,
    listing_filters: Arc<Mutex<Vec<Option<String>>>>,
    /// Every subject digest a referrer listing was addressed with, in call
    /// order. A second pass over a different subject is otherwise invisible
    /// from outside: this double serves one referrer set regardless of subject,
    /// so only the addressing says how many subjects were read.
    listed_subjects: Arc<Mutex<Vec<Digest>>>,
    pulled_blobs: Arc<Mutex<Vec<String>>>,
}

impl SbomTransport {
    pub fn new(subject_manifest: &[u8], referrers: Vec<ServedReferrer>) -> Self {
        Self {
            subject_manifest: subject_manifest.to_vec(),
            referrers,
            sidecar: None,
            listing_filters: Arc::new(Mutex::new(Vec::new())),
            listed_subjects: Arc::new(Mutex::new(Vec::new())),
            pulled_blobs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Hang a `sha256-<hex>.sbom` sidecar off the subject, one document per
    /// layer the manifest declares.
    #[must_use]
    pub fn with_sidecar(mut self, manifest: &[u8], documents: &[&[u8]]) -> Self {
        self.sidecar = Some(SbomSidecar {
            manifest: manifest.to_vec(),
            documents: documents.iter().map(|body| body.to_vec()).collect(),
        });
        self
    }

    pub fn listed_subjects(&self) -> Vec<Digest> {
        self.listed_subjects.lock().expect("recorder lock").clone()
    }

    pub fn pulled_blobs(&self) -> Vec<String> {
        self.pulled_blobs.lock().expect("recorder lock").clone()
    }

    pub fn listing_filters(&self) -> Vec<Option<String>> {
        self.listing_filters.lock().expect("recorder lock").clone()
    }

    fn subject_digest(&self) -> Digest {
        Algorithm::Sha256.hash(&self.subject_manifest)
    }
}

#[async_trait]
impl OciTransport for SbomTransport {
    async fn ensure_auth(&self, _: &Reference, _: RegistryOperation) -> Answer<()> {
        Ok(())
    }

    async fn list_tags(&self, _: &Reference, _: usize, _: Option<String>) -> Answer<Vec<String>> {
        unimplemented!("the sbom scan never lists tags")
    }

    async fn catalog(&self, _: &Reference, _: usize, _: Option<String>) -> Answer<Vec<String>> {
        unimplemented!("the sbom scan never reads the catalog")
    }

    async fn fetch_manifest_digest(&self, _: &Reference) -> Answer<String> {
        unimplemented!("the sbom scan resolves digests through the index")
    }

    async fn pull_manifest_raw(&self, image: &Reference, _: &[&str]) -> Answer<(Vec<u8>, String)> {
        let subject = self.subject_digest();
        if image.digest() == Some(subject.to_string().as_str()) {
            return Ok((self.subject_manifest.clone(), subject.to_string()));
        }
        // Tag-addressed: the `sha256-<hex>.att` and `.sbom` sidecar doors. The
        // `.sbom` one is served when a test hung a sidecar on the subject;
        // everything else 404s, which is what a real registry does and what the
        // readers must read as "no legacy artifact" — modelled here rather than
        // panicked on, so a scan that legitimately tries a door does not fail a
        // test about something else. Every *digest*-addressed read still has to
        // be one this transport listed.
        let Some(wanted) = image.digest().map(str::to_owned) else {
            if let (Some(tag), Some(sidecar)) = (image.tag(), self.sidecar.as_ref())
                && tag.ends_with(".sbom")
            {
                return Ok((sidecar.manifest.clone(), sidecar.digest().to_string()));
            }
            return Err(ClientError::ManifestNotFound(image.to_string()));
        };
        let referrer = self
            .referrers
            .iter()
            .find(|served| served.descriptor.digest == wanted)
            .expect("the scan only asks for referrers this transport listed");
        Ok((referrer.manifest.clone(), wanted))
    }

    async fn pull_blob(&self, _: &Reference, _: &Digest) -> Answer<Vec<u8>> {
        unimplemented!("the sbom scan streams blobs")
    }

    async fn pull_blob_streaming(
        &self,
        _: &Reference,
        digest: &Digest,
    ) -> Answer<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>> {
        self.pulled_blobs
            .lock()
            .expect("recorder lock")
            .push(digest.to_string());
        if let Some(sidecar) = self.sidecar.as_ref()
            && let Some(document) = sidecar.document_for(&digest.to_string())
        {
            return Ok(Box::new(std::io::Cursor::new(document)));
        }
        let document = self
            .referrers
            .iter()
            .find_map(|served| served.blob_for(digest))
            .expect("the scan only asks for blobs a listed referrer named");
        Ok(Box::new(std::io::Cursor::new(document)))
    }

    async fn pull_blob_to_file(&self, _: &Reference, _: &Digest, _: &Path) -> Answer<()> {
        unimplemented!("the sbom scan never writes blobs to disk")
    }

    async fn head_blob(&self, _: &Reference, _: &Digest) -> Answer<u64> {
        unimplemented!("the sbom scan never HEADs blobs")
    }

    async fn push_manifest(&self, _: &Reference, _: &Manifest) -> Answer<String> {
        unimplemented!("reading an SBOM never pushes")
    }

    async fn push_manifest_raw(&self, _: &Reference, _: Vec<u8>, _: &str) -> Answer<String> {
        unimplemented!("reading an SBOM never pushes")
    }

    async fn push_blob(&self, _: &Reference, _: Vec<u8>, _: &Digest, _: ProgressFn) -> Answer<String> {
        unimplemented!("reading an SBOM never pushes")
    }

    async fn push_blob_from_path(&self, _: &Reference, _: &Path, _: &Digest, _: ProgressFn) -> Answer<String> {
        unimplemented!("reading an SBOM never pushes a file-backed blob")
    }

    async fn push_referrer_manifest(&self, _: &Reference, _: &Digest, _: &[u8], _: &str) -> Answer<Descriptor> {
        unimplemented!("reading an SBOM never pushes")
    }

    async fn list_referrers(
        &self,
        _: &Reference,
        subject: &Digest,
        artifact_type: Option<&str>,
    ) -> Answer<Vec<Descriptor>> {
        self.listing_filters
            .lock()
            .expect("recorder lock")
            .push(artifact_type.map(str::to_string));
        self.listed_subjects
            .lock()
            .expect("recorder lock")
            .push(subject.clone());
        Ok(self
            .referrers
            .iter()
            .filter(|served| {
                artifact_type.is_none_or(|wanted| served.descriptor.artifact_type.as_deref() == Some(wanted))
            })
            .map(|served| served.descriptor.clone())
            .collect())
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}

impl crate::sealed::Sealed for SbomTransport {}
