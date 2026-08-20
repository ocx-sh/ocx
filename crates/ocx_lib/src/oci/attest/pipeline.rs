// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Attest pipeline — the push-side state machine for in-toto attestations.
//!
//! Per
//! [`adr_sbom_attestations.md`](../../../../../.claude/artifacts/adr_sbom_attestations.md)
//! (C-009): refuse offline → resolve and floor-check the predicate type →
//! SSRF floor on both trust services → resolve the per-platform target → index
//! indirection → referrers capability probe → acquire an OIDC token → build
//! the in-toto Statement → DSSE-sign it → push the bundle blob → push the
//! referrer manifest whose `subject` points at the target.
//!
//! The floor check sits at step 1 rather than after token acquisition, where
//! the design record's narrative puts it. Deliberate: the refusal is a pure
//! function of `--type` and exits 64, so the later position would make a user
//! finish an OAuth flow to be told the invocation was wrong — a usage error
//! belongs before the credential, not after it.
//!
//! Mirrors [`SignPipeline`](crate::oci::sign::SignPipeline) field-for-field
//! where the concerns are identical, so the two read as siblings. It diverges
//! exactly where the protocols do: what is signed (an in-toto Statement, not a
//! bare digest), what the bundle carries (`dsseEnvelope`), and the third
//! referrer annotation (`dev.sigstore.bundle.predicateType`) that a signature
//! has no value for.
//!
//! No fallback tag is ever written (ADR S1-F, inherited unchanged).

use serde_json::value::RawValue;
use url::Url;

use super::predicate::{self, PredicateType};
use super::statement;
use crate::file_structure::StateStore;
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport};
use crate::oci::index::{Index, IndexOperation, SelectResult};
use crate::oci::referrer::ReferrerManifest;
use crate::oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use crate::oci::referrer::manifest::{bundle_annotations, bundle_created, bundle_now};
use crate::oci::referrer::media_types::{
    BUNDLE_CONTENT_DSSE, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_PAYLOAD, SIGSTORE_BUNDLE_V03,
};
use crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE;
use crate::oci::sign::{SignError, SignErrorKind, Signer, TokenProvider};
use crate::oci::{Descriptor, Digest, Identifier, OCI_IMAGE_MEDIA_TYPE, Platform, native};

/// Manifest media types accepted when fetching the per-platform target.
const ACCEPTED_MANIFEST_TYPES: &[&str] = &[
    OCI_IMAGE_MEDIA_TYPE,
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// Context passed into [`AttestPipeline::run`] — all external dependencies.
pub struct AttestContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Platform selector for multi-platform manifests.
    pub platform: &'a Platform,
    /// Signer producing the DSSE-enveloped bundle.
    pub signer: &'a dyn Signer,
    /// OIDC token provider (override → ambient → browser dispatch).
    pub token_provider: &'a dyn TokenProvider,
    /// The requested predicate type; its resolved URI is what gets written.
    pub predicate_type: &'a PredicateType,
    /// The predicate file's bytes, validated as JSON and otherwise untouched (D-b).
    pub predicate: &'a RawValue,
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// Present so the S1-E policy refusal can run here rather than being
    /// reinvented per call site — it is step 0 of [`AttestPipeline::run`].
    pub offline: bool,
    /// Index for resolving tag → per-platform manifest digest.
    pub index: &'a Index,
    /// Fulcio URL (validated at the CLI boundary).
    pub fulcio_url: &'a Url,
    /// Rekor URL (validated at the CLI boundary).
    pub rekor_url: &'a Url,
    /// State store owning the referrers-capability cache layout.
    pub state: &'a StateStore,
}

/// Result emitted by a successful attest pipeline run.
#[derive(Debug)]
pub struct AttestResult {
    /// Digest of the target manifest the attestation was attached to.
    pub subject_digest: Digest,
    /// The RESOLVED predicateType URI (D-c) — echoed in the report so alias
    /// resolution is visible rather than surprising.
    pub predicate_type: String,
    /// Digest of the pushed Sigstore bundle blob.
    pub bundle_digest: Digest,
    /// Digest of the pushed referrer manifest.
    pub referrer_digest: Digest,
    /// Full OCI descriptor of the pushed referrer manifest.
    pub referrer_descriptor: Descriptor,
    /// Cert SAN (identity) that signed the Statement — the OIDC subject.
    pub certificate_identity: String,
    /// Cert issuer (`--certificate-oidc-issuer` comparand) — the OIDC issuer.
    pub certificate_oidc_issuer: String,
}

/// Attest pipeline entry point.
pub struct AttestPipeline;

impl AttestPipeline {
    /// Run the push-side attest state machine.
    ///
    /// The registry transport is derived from `client` internally, so the
    /// public API never exposes `&dyn OciTransport` — the same seam
    /// [`SignPipeline::run`](crate::oci::sign::SignPipeline::run) uses.
    ///
    /// # Errors
    ///
    /// [`SignError`] tagged with the target identifier. Notable kinds:
    /// [`SignErrorKind::OfflineAttestRefused`] (77) before any credential is
    /// touched, [`SignErrorKind::ProvenanceVersionUnsupported`] (64) when the
    /// requested `--type` resolves to provenance below v1.0, and
    /// [`SignErrorKind::ReferrersUnsupported`] (84) on a registry without the
    /// OCI Referrers API.
    pub async fn run(client: &Client, ctx: AttestContext<'_>) -> Result<AttestResult, SignError> {
        let identifier = ctx.identifier.clone();
        Self::run_inner(client, ctx)
            .await
            .map_err(|kind| SignError::new(identifier, kind))
    }

    async fn run_inner(client: &Client, ctx: AttestContext<'_>) -> Result<AttestResult, SignErrorKind> {
        // 0. S1-E policy refusal, ahead of every credential read and every
        //    socket. Attesting is signing, so an offline attest is a deliberate
        //    rejection (77) rather than a passive transport failure, and it
        //    must not depend on which verb reached here — `package attest` and
        //    `package push --sbom` both route through this line.
        if ctx.offline {
            return Err(SignErrorKind::OfflineAttestRefused);
        }

        // 1. The attach-side SLSA floor (#102, checklist row 21). Deliberately
        //    before the network rather than after token acquisition: the answer
        //    is a pure function of `--type`, and exit 64 says the *invocation*
        //    is wrong. Making the user finish an OAuth flow to be told so both
        //    touches a credential for a run that was never going to succeed
        //    and makes the negative fixture depend on a live Fulcio.
        //
        //    Dispatch is on the RESOLVED URI, so a full-URI `--type` spelling
        //    hits the floor exactly as the alias does.
        let predicate_type = ctx.predicate_type.uri().to_owned();
        if predicate::is_provenance_below_v1(ctx.predicate_type) {
            return Err(SignErrorKind::ProvenanceVersionUnsupported {
                resolved: predicate_type,
            });
        }

        // 2. SSRF floor for the trust services (CWE-918). The CLI boundary
        //    validated these URLs as *strings*; this is where we find out where
        //    they actually resolve, before anything dials them. Attesting
        //    always reaches Fulcio and Rekor, so both are guarded.
        let trusted = ctx.index.trusted_hosts_for(ctx.identifier.registry());
        for (url, flag) in [(ctx.fulcio_url, "--fulcio-url"), (ctx.rekor_url, "--rekor-url")] {
            crate::oci::endpoint::resolve_sigstore_url(url, trusted)
                .await
                .map_err(|error| SignErrorKind::InvalidEndpointUrl {
                    endpoint: flag.into(),
                    reason: crate::oci::endpoint::UrlRejection::from(error),
                })?;
        }

        let transport = client.transport();
        // 3. Resolve the per-platform target manifest. This digest is the
        //    subject — never one derived from a canonical tag, which
        //    `--no-canonical-tag` may have suppressed (D-f).
        let resolved = match ctx
            .index
            .select(ctx.identifier, ctx.platform, IndexOperation::Resolve)
            .await
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?
        {
            SelectResult::Found(id) => id,
            SelectResult::Ambiguous(_) | SelectResult::NotFound | SelectResult::FeatureMismatch { .. } => {
                return Err(SignErrorKind::TargetNotFound {
                    platform: ctx.platform.to_string(),
                });
            }
        };
        let subject_digest = resolved
            .digest()
            .ok_or_else(|| SignErrorKind::Internal("resolved target has no digest".into()))?;
        // Index indirection: a logical name (`ocx.sh/<ns>/<pkg>`) may point at
        // a different physical registry, so every transport-facing call below
        // targets the physical address. Same contract the sign pipeline reads;
        // the SSRF floor on the returned host is enforced upstream in the
        // shared index choke point.
        let physical = ctx
            .index
            .physical_reference(&resolved)
            .await
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?
            .unwrap_or_else(|| resolved.clone());
        // The upstream pre-flight tolerates a DNS lookup failure by design; the
        // dial site is where the guard is fail-closed, and a request is now
        // imminent (CWE-918).
        ctx.index
            .guard_physical_dial(&resolved, &physical)
            .await
            .map_err(|error| SignErrorKind::ForbiddenRegistryTarget {
                reason: error.to_string(),
            })?;
        // Two seams, because attesting both reads and writes: the subject fetch
        // may be served by a `[mirrors]` entry, but the referrer push must
        // reach the canonical host — remote/proxy mirrors are read-only (ADR
        // Q5), so an attestation pushed at a mirror is rejected, or lands where
        // the canonical verifier never looks.
        let read_image = client.transport_reference(&physical);
        let write_image = client.transport_write_reference(&physical);

        // Fetch the target manifest bytes for the subject descriptor's size.
        // `clone_with_digest` drops the tag, so this stays digest-only — a
        // `repo:tag@digest` reference keys a different registry path and 404s.
        let subject_ref = read_image.clone_with_digest(subject_digest.to_string());
        let (subject_bytes, served_digest) = transport
            .pull_manifest_raw(&subject_ref, ACCEPTED_MANIFEST_TYPES)
            .await
            .map_err(map_client_error)?;
        // The bytes came from the READ host but their length becomes the
        // subject descriptor's `size` pushed to the CANONICAL one. Bind them to
        // the digest already resolved before trusting that length — a wrong
        // size yields an attestation strict verifiers reject.
        if Digest::try_from(served_digest.as_str()).ok().as_ref() != Some(&subject_digest) {
            return Err(map_client_error(ClientError::DigestMismatch {
                expected: subject_digest.to_string(),
                actual: served_digest,
            }));
        }

        // 4. Referrers-API capability (cache-first) of the host we will PUSH
        //    to. A mirror's referrers support says nothing about the upstream's.
        Self::ensure_referrers_supported(transport, &ctx, &write_image, &subject_digest).await?;

        // 5. Acquire the OIDC token.
        let token = ctx.token_provider.acquire("sigstore").await?;

        // 6. Build the in-toto Statement and sign it as a DSSE envelope. One
        //    instant serves both the signed cosign wrapper and the `created`
        //    annotation below, so a `SOURCE_DATE_EPOCH` run stamps the same
        //    value in both places.
        let now = bundle_now();
        let statement = statement::build(
            physical.repository(),
            &subject_digest,
            ctx.predicate_type,
            ctx.predicate,
            now,
        )?;
        let statement_bytes = serde_json::to_vec(&statement).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let bundle = ctx
            .signer
            .sign_dsse(&statement_bytes, &token, ctx.fulcio_url, ctx.rekor_url)
            .await?;

        // 7. Push the referrer's blobs: the OCI empty-config blob (the
        //    manifest's `config` descriptor points at it) and the bundle blob
        //    (the `layers[0]` payload). A spec-strict registry rejects the
        //    manifest with MANIFEST_INVALID if either is absent, so both land
        //    before the manifest PUT.
        let no_progress: std::sync::Arc<dyn Fn(u64) + Send + Sync> = std::sync::Arc::new(|_| ());
        let empty_config_digest =
            Digest::try_from(EMPTY_CONFIG_DIGEST).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        transport
            .push_blob(
                &write_image,
                EMPTY_CONFIG_PAYLOAD.to_vec(),
                &empty_config_digest,
                no_progress.clone(),
            )
            .await
            .map_err(map_client_error)?;
        transport
            .push_blob(&write_image, bundle.bytes.clone(), &bundle.digest, no_progress)
            .await
            .map_err(map_client_error)?;

        // 8. Build + push the referrer manifest (subject -> target).
        let subject_descriptor = Descriptor {
            media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: subject_digest.to_string(),
            size: subject_bytes.len() as i64,
            ..Descriptor::default()
        };
        let bundle_descriptor = Descriptor {
            media_type: BUNDLE_V03_MEDIA_TYPE.to_string(),
            digest: bundle.digest.to_string(),
            size: bundle.bytes.len() as i64,
            ..Descriptor::default()
        };
        // cosign parity (ADR D1): an attestation referrer carries the same
        // `artifactType` a signature does and is told apart by
        // `content: dsse-envelope`, with the RESOLVED predicateType as the
        // third annotation. The set is a one-way door — the manifest's SHA-256
        // *is* the referrer's registry address.
        let annotations = bundle_annotations(&bundle_created(now), BUNDLE_CONTENT_DSSE, Some(&predicate_type));
        let manifest = ReferrerManifest::build(
            subject_descriptor,
            SIGSTORE_BUNDLE_V03,
            bundle_descriptor,
            Some(annotations),
        );
        let manifest_bytes = manifest.to_canonical_json()?;
        let referrer_descriptor = transport
            .push_referrer_manifest(&write_image, &subject_digest, &manifest_bytes, OCI_IMAGE_MEDIA_TYPE)
            .await
            .map_err(map_client_error)?;
        let referrer_digest =
            Digest::try_from(referrer_descriptor.digest.as_str()).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        Ok(AttestResult {
            subject_digest,
            predicate_type,
            bundle_digest: bundle.digest,
            referrer_digest,
            referrer_descriptor,
            certificate_identity: bundle.certificate_identity,
            certificate_oidc_issuer: bundle.certificate_oidc_issuer,
        })
    }

    /// Confirm the registry serves the OCI Referrers API, consulting (and
    /// refreshing) the per-registry capability cache. `Unsupported` ->
    /// [`SignErrorKind::ReferrersUnsupported`] (exit 84).
    async fn ensure_referrers_supported(
        transport: &dyn OciTransport,
        ctx: &AttestContext<'_>,
        image: &native::Reference,
        subject_digest: &Digest,
    ) -> Result<(), SignErrorKind> {
        // Cache key = the host actually probed (`probe` records the same one),
        // so a mirrored registry caches under the mirror, not the upstream.
        let cached = if ctx.no_cache {
            None
        } else {
            ReferrersApiCapability::from_cache(image.resolve_registry(), ctx.state)
                .await
                .ok()
                .flatten()
                .filter(ReferrersApiCapability::is_fresh)
        };
        let capability = match cached {
            Some(hit) => hit,
            None => {
                let probed = ReferrersApiCapability::probe(transport, image, subject_digest)
                    .await
                    .map_err(map_client_error)?;
                // Best-effort cache write; a failure here must not fail the attach.
                let _ = probed.write_cache(ctx.state).await;
                probed
            }
        };
        match capability.supported {
            ReferrersSupport::Supported => Ok(()),
            ReferrersSupport::Unsupported => Err(SignErrorKind::ReferrersUnsupported),
        }
    }
}

/// Map an OCI client error into the sign taxonomy.
///
/// Identical policy to the sign pipeline's: only `ReferrersUnsupported` has a
/// faithful attest-side kind. Everything else keeps its `ClientError` intact
/// under `Internal` rather than being flattened into a kind that would
/// misdescribe it — `SignError::classify` defers on `Internal`, so the wrapped
/// cause supplies its own exit code (401 -> 80, 5xx -> 69, transient -> 75).
fn map_client_error(error: ClientError) -> SignErrorKind {
    match error {
        ClientError::ReferrersUnsupported { .. } => SignErrorKind::ReferrersUnsupported,
        other => SignErrorKind::Internal(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::cli::{ExitCode, classify_error};
    use crate::oci::Algorithm;
    use crate::oci::sign::bundle::SignedBundle;
    use crate::oci::sign::oidc::OidcToken;

    /// SHA-256 the indirecting test index reports as the subject digest.
    fn subject_digest() -> Digest {
        Algorithm::Sha256.hash(b"attested subject manifest")
    }

    /// A test index whose logical name resolves to a DIFFERENT physical
    /// registry — the `index.ocx.sh` shape reduced to what the pipeline reads.
    #[derive(Clone)]
    struct IndirectingIndex {
        physical: Identifier,
    }

    #[async_trait::async_trait]
    impl crate::oci::index::IndexImpl for IndirectingIndex {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            _: &Identifier,
            _: IndexOperation,
        ) -> crate::Result<Option<(Digest, crate::oci::Manifest)>> {
            Ok(Some((
                subject_digest(),
                crate::oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )))
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(subject_digest()))
        }

        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        async fn physical_reference(&self, _: &Identifier) -> crate::Result<Option<Identifier>> {
            Ok(Some(self.physical.clone()))
        }

        fn box_clone(&self) -> Box<dyn crate::oci::index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// What the pipeline actually wrote, captured for assertion: the
    /// `"<method>:<registry>"` call log and the referrer manifest bytes.
    ///
    /// The sign pipeline's double records call sites only; the attestation
    /// contract is in the manifest *body* (D1's artifactType plus three
    /// annotations), so this one keeps the bytes.
    #[derive(Clone, Default)]
    struct RecordingTransport {
        calls: Arc<Mutex<Vec<String>>>,
        referrer_manifests: Arc<Mutex<Vec<Vec<u8>>>>,
        /// When set, `pull_manifest_raw` claims this digest instead of the
        /// resolved subject digest — a mirror serving the wrong manifest.
        served_subject_digest: Option<String>,
    }

    impl RecordingTransport {
        fn record(&self, method: &str, image: &native::Reference) {
            self.calls
                .lock()
                .expect("recorder lock")
                .push(format!("{method}:{}", image.resolve_registry()));
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("recorder lock").clone()
        }

        /// The one referrer manifest a successful run pushes, as JSON.
        fn pushed_referrer(&self) -> serde_json::Value {
            let pushed = self.referrer_manifests.lock().expect("recorder lock").clone();
            let [bytes] = pushed.as_slice() else {
                panic!("expected exactly one referrer manifest push, got {}", pushed.len());
            };
            serde_json::from_slice(bytes).expect("referrer manifest is JSON")
        }
    }

    #[async_trait::async_trait]
    impl OciTransport for RecordingTransport {
        async fn ensure_auth(
            &self,
            image: &native::Reference,
            _: crate::oci::RegistryOperation,
        ) -> std::result::Result<(), ClientError> {
            self.record("ensure_auth", image);
            Ok(())
        }

        async fn list_tags(
            &self,
            _: &native::Reference,
            _: usize,
            _: Option<String>,
        ) -> std::result::Result<Vec<String>, ClientError> {
            unimplemented!("attest never lists tags")
        }

        async fn catalog(
            &self,
            _: &native::Reference,
            _: usize,
            _: Option<String>,
        ) -> std::result::Result<Vec<String>, ClientError> {
            unimplemented!("attest never reads the catalog")
        }

        async fn fetch_manifest_digest(&self, _: &native::Reference) -> std::result::Result<String, ClientError> {
            unimplemented!("attest resolves digests through the index")
        }

        async fn pull_manifest_raw(
            &self,
            image: &native::Reference,
            _: &[&str],
        ) -> std::result::Result<(Vec<u8>, String), ClientError> {
            self.record("pull_manifest_raw", image);
            let digest = self
                .served_subject_digest
                .clone()
                .unwrap_or_else(|| subject_digest().to_string());
            Ok((b"{}".to_vec(), digest))
        }

        async fn pull_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<Vec<u8>, ClientError> {
            unimplemented!("attest never pulls blobs")
        }

        async fn pull_blob_to_file(
            &self,
            _: &native::Reference,
            _: &Digest,
            _: &std::path::Path,
        ) -> std::result::Result<(), ClientError> {
            unimplemented!("attest never pulls blobs")
        }

        async fn head_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<u64, ClientError> {
            unimplemented!("attest never HEADs blobs")
        }

        async fn push_manifest(
            &self,
            _: &native::Reference,
            _: &crate::oci::Manifest,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("attest pushes through push_referrer_manifest")
        }

        async fn push_manifest_raw(
            &self,
            _: &native::Reference,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("attest pushes through push_referrer_manifest")
        }

        async fn push_blob(
            &self,
            image: &native::Reference,
            _: Vec<u8>,
            digest: &Digest,
            _: Arc<dyn Fn(u64) + Send + Sync>,
        ) -> std::result::Result<String, ClientError> {
            self.record("push_blob", image);
            Ok(digest.to_string())
        }

        async fn push_referrer_manifest(
            &self,
            image: &native::Reference,
            _: &Digest,
            manifest_bytes: &[u8],
            media_type: &str,
        ) -> std::result::Result<Descriptor, ClientError> {
            self.record("push_referrer_manifest", image);
            self.referrer_manifests
                .lock()
                .expect("recorder lock")
                .push(manifest_bytes.to_vec());
            Ok(Descriptor {
                media_type: media_type.to_string(),
                digest: Algorithm::Sha256.hash(manifest_bytes).to_string(),
                size: manifest_bytes.len() as i64,
                ..Descriptor::default()
            })
        }

        async fn list_referrers(
            &self,
            image: &native::Reference,
            _: &Digest,
            _: Option<&str>,
        ) -> std::result::Result<Vec<Descriptor>, ClientError> {
            // A successful (empty) listing is what the capability probe reads
            // as "this registry supports the Referrers API".
            self.record("list_referrers", image);
            Ok(Vec::new())
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.clone())
        }
    }

    /// Build an unsigned JWT (`header.payload.sig`) whose payload is `claims`.
    fn jwt_with_payload(claims: &serde_json::Value) -> String {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("eyJhbGciOiJFUzI1NiJ9.{payload}.sig")
    }

    /// Counts acquisitions, so a refusal test can assert the credential path
    /// was never entered — not merely that no request was sent.
    #[derive(Clone, Default)]
    struct CountingTokenProvider {
        acquisitions: Arc<Mutex<usize>>,
    }

    #[async_trait::async_trait]
    impl TokenProvider for CountingTokenProvider {
        async fn acquire(&self, _: &str) -> Result<OidcToken, SignErrorKind> {
            *self.acquisitions.lock().expect("counter lock") += 1;
            Ok(OidcToken::new(jwt_with_payload(
                &serde_json::json!({ "sub": "me@example.com", "iss": "https://issuer.example" }),
            )))
        }
    }

    /// Captures the exact bytes handed to `sign_dsse` — the Statement the
    /// pipeline built is only observable here.
    #[derive(Clone, Default)]
    struct RecordingSigner {
        signed: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl RecordingSigner {
        /// The one Statement a successful run signs, as JSON.
        fn signed_statement(&self) -> serde_json::Value {
            let signed = self.signed.lock().expect("recorder lock").clone();
            let [bytes] = signed.as_slice() else {
                panic!("expected exactly one sign_dsse call, got {}", signed.len());
            };
            serde_json::from_slice(bytes).expect("statement is JSON")
        }
    }

    #[async_trait::async_trait]
    impl Signer for RecordingSigner {
        async fn sign(&self, _: &Digest, _: &OidcToken, _: &Url, _: &Url) -> Result<SignedBundle, SignErrorKind> {
            unreachable!("AttestPipeline signs DSSE envelopes, never message signatures")
        }

        async fn sign_dsse(
            &self,
            statement_bytes: &[u8],
            _: &OidcToken,
            _: &Url,
            _: &Url,
        ) -> Result<SignedBundle, SignErrorKind> {
            self.signed
                .lock()
                .expect("recorder lock")
                .push(statement_bytes.to_vec());
            let bytes = br#"{"mediaType":"test-dsse-bundle"}"#.to_vec();
            let digest = Algorithm::Sha256.hash(&bytes);
            Ok(SignedBundle {
                bytes,
                digest,
                certificate_identity: "me@example.com".to_string(),
                certificate_oidc_issuer: "https://issuer.example".to_string(),
            })
        }

        fn signer_kind(&self) -> &'static str {
            "test-recording"
        }
    }

    /// Everything a driven run produced, so each test reads only what it asserts.
    struct Run {
        result: Result<AttestResult, SignError>,
        transport: RecordingTransport,
        signer: RecordingSigner,
        acquisitions: usize,
        /// Held so the state dir outlives the assertions.
        _state: tempfile::TempDir,
    }

    /// Drive a full attest run for the logical name `ocx.sh/acme/tool:1.0`,
    /// indirected to the physical `8.8.8.8/acme/tool:1.0`.
    ///
    /// `predicate` is the verbatim JSON text the caller's `--predicate` file
    /// would have held.
    async fn drive_attest(predicate_type: PredicateType, predicate: &str, offline: bool) -> Run {
        drive_attest_with(RecordingTransport::default(), predicate_type, predicate, offline).await
    }

    /// `drive_attest` with the transport injected, so a test can drive one that
    /// misbehaves.
    async fn drive_attest_with(
        transport: RecordingTransport,
        predicate_type: PredicateType,
        predicate: &str,
        offline: bool,
    ) -> Run {
        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        // A public IP literal, not a name: the pipeline resolves the physical
        // host before dialing it (dial-site SSRF guard), and a DNS name here
        // would make this unit test open a socket.
        let physical = Identifier::parse("8.8.8.8/acme/tool:1.0").expect("physical identifier");

        let client = Client::with_transport(Box::new(transport.clone()));
        let index = Index::from_impl(IndirectingIndex { physical });
        let state_dir = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(state_dir.path());
        // Loopback rather than a `.example` name: the dial-time SSRF guard
        // resolves the endpoint before use, and a documentation domain does not
        // resolve — which would make this unit test depend on DNS.
        let fulcio_url = Url::parse("http://127.0.0.1:5555").expect("fulcio url");
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let platform = Platform::any();
        let signer = RecordingSigner::default();
        let token_provider = CountingTokenProvider::default();
        let predicate: Box<RawValue> = serde_json::from_str(predicate).expect("predicate is JSON");

        let result = AttestPipeline::run(
            &client,
            AttestContext {
                identifier: &logical,
                platform: &platform,
                signer: &signer,
                token_provider: &token_provider,
                predicate_type: &predicate_type,
                predicate: &predicate,
                no_cache: true,
                offline,
                index: &index,
                fulcio_url: &fulcio_url,
                rekor_url: &rekor_url,
                state: &state,
            },
        )
        .await;

        let acquisitions = *token_provider.acquisitions.lock().expect("counter lock");
        Run {
            result,
            transport,
            signer,
            acquisitions,
            _state: state_dir,
        }
    }

    /// A minimal SBOM-shaped predicate; the pipeline never reads inside it.
    const PREDICATE: &str = r#"{"bomFormat":"CycloneDX","specVersion":"1.6"}"#;

    async fn drive_ok(predicate_type: PredicateType) -> Run {
        let run = drive_attest(predicate_type, PREDICATE, false).await;
        if let Err(error) = &run.result {
            panic!("attest must complete against the recording transport: {error}");
        }
        run
    }

    fn annotations(manifest: &serde_json::Value) -> BTreeMap<String, String> {
        manifest["annotations"]
            .as_object()
            .expect("referrer manifest carries annotations")
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.as_str().expect("annotation value is a string").to_string(),
                )
            })
            .collect()
    }

    // ── S-002: the offline policy refusal ──────────────────────────────────

    /// S1-E: an offline attest is a deliberate policy rejection (77), not a
    /// passive transport failure — and it must land before the credential path
    /// is entered, or a refused run has already touched an OIDC token.
    #[tokio::test]
    async fn attest_refuses_offline_before_any_traffic_or_token() {
        let run = drive_attest(PredicateType::CycloneDx, PREDICATE, true).await;

        let Err(error) = run.result else {
            panic!("an offline attest must be refused");
        };
        assert!(
            matches!(error.kind, SignErrorKind::OfflineAttestRefused),
            "expected the offline policy refusal, got: {error}",
        );
        assert_eq!(classify_error(&error), ExitCode::PermissionDenied);
        assert!(
            run.transport.calls().is_empty(),
            "no transport call may precede the refusal, got: {:?}",
            run.transport.calls(),
        );
        assert_eq!(run.acquisitions, 0, "a refused run must not resolve a token");
    }

    // ── S-003: the SLSA provenance >= v1.0 attach floor (#102, row 21) ──────

    /// Every spelling that RESOLVES to provenance v0.2 hits the floor —
    /// including a full URI, which is why the check dispatches on the resolved
    /// URI rather than on the variant.
    #[tokio::test]
    async fn attest_refuses_every_spelling_of_provenance_below_v1() {
        let below_v1 = [
            PredicateType::SlsaProvenance,
            PredicateType::SlsaProvenance02,
            PredicateType::Uri("https://slsa.dev/provenance/v0.2".to_string()),
        ];
        for predicate_type in below_v1 {
            let run = drive_attest(predicate_type.clone(), PREDICATE, false).await;
            let Err(error) = run.result else {
                panic!("{predicate_type:?} resolves below v1.0 and must be refused");
            };
            let SignErrorKind::ProvenanceVersionUnsupported { resolved } = &error.kind else {
                panic!("expected the provenance floor refusal for {predicate_type:?}, got: {error}");
            };
            assert_eq!(resolved, "https://slsa.dev/provenance/v0.2");
            // 64, not 65: the offending value came from the invocation, so the
            // fix is a different flag value rather than a different file.
            assert_eq!(classify_error(&error), ExitCode::UsageError, "for {predicate_type:?}");
            // `SignError`'s own Display is the identifier; the sentence the
            // user acts on is the kind's, which rides the `source()` chain.
            assert!(
                error.kind.to_string().contains("--type slsaprovenance1"),
                "the refusal must name the fix, got: {}",
                error.kind,
            );
            assert!(
                run.transport.calls().is_empty(),
                "the floor must refuse before any traffic, got: {:?}",
                run.transport.calls(),
            );
            assert_eq!(run.acquisitions, 0, "a refused run must not resolve a token");
        }
    }

    /// The converse half: the floor is scoped to provenance below v1.0 and
    /// nothing else. Without this, a floor that refused every type would pass
    /// the test above.
    #[tokio::test]
    async fn attest_accepts_provenance_at_v1_and_every_non_provenance_type() {
        let accepted = [
            PredicateType::SlsaProvenance1,
            PredicateType::Uri("https://slsa.dev/provenance/v1".to_string()),
            PredicateType::CycloneDx,
            PredicateType::Custom,
        ];
        for predicate_type in accepted {
            let run = drive_attest(predicate_type.clone(), PREDICATE, false).await;
            assert!(
                run.result.is_ok(),
                "{predicate_type:?} must pass the attach floor, got: {:?}",
                run.result.err().map(|e| e.to_string()),
            );
        }
    }

    // ── D-f: the subject is the digest OCX resolved, not a derived tag ──────

    /// The signed Statement's subject digest is the per-platform manifest
    /// digest the index resolved. A subject taken from anywhere else — a
    /// canonical tag, the logical reference — attests to an artifact the
    /// verifier will not be looking at.
    #[tokio::test]
    async fn attest_signs_a_statement_bound_to_the_resolved_subject_digest() {
        let run = drive_ok(PredicateType::CycloneDx).await;
        let statement = run.signer.signed_statement();

        let subjects = statement["subject"].as_array().expect("statement carries subjects");
        assert_eq!(subjects.len(), 1, "one subject: the digest OCX resolved");
        assert_eq!(
            subjects[0]["digest"]["sha256"].as_str(),
            Some(subject_digest().hex()),
            "the subject must be the resolved per-platform manifest digest",
        );
        assert_eq!(
            statement["_type"].as_str(),
            Some("https://in-toto.io/Statement/v1"),
            "OCX writes Statement v1",
        );
        // Positive control for the counter the two refusal tests assert is
        // zero. Without it a broken increment leaves both of them green and
        // vacuous, and the transport recorder cannot stand in — token
        // acquisition never routes through `OciTransport`.
        assert_eq!(run.acquisitions, 1, "a completed run resolves exactly one token");

        let result = run.result.expect("run succeeded");
        assert_eq!(result.subject_digest, subject_digest());
    }

    /// The predicate travels verbatim: whatever the file held is what gets
    /// signed, with no re-serialization in between (D-b, checklist row 2).
    #[tokio::test]
    async fn attest_signs_the_predicate_bytes_verbatim() {
        // Spelled so a `Value` round-trip is observable: a trailing-zero float
        // and a non-alphabetical key order.
        const SPELLED: &str = r#"{"zeta":1,"alpha":1.50}"#;
        let run = drive_attest(PredicateType::CycloneDx, SPELLED, false).await;
        run.result.expect("run succeeded");

        let signed = run.signer.signed.lock().expect("recorder lock").clone();
        let [bytes] = signed.as_slice() else {
            panic!("expected exactly one sign_dsse call");
        };
        let text = String::from_utf8(bytes.clone()).expect("statement is UTF-8");
        assert!(
            text.contains(SPELLED),
            "the predicate must be spliced verbatim, got: {text}",
        );
    }

    // ── D1: the referrer wire shape ────────────────────────────────────────

    /// cosign parity: an attestation referrer shares the signature referrer's
    /// `artifactType` and is told apart by `dev.sigstore.bundle.content`. The
    /// third annotation carries the RESOLVED predicateType URI — `spdxjson`
    /// resolves to the SPDX URI, so an implementation echoing the alias
    /// spelling instead fails here.
    #[tokio::test]
    async fn attest_writes_the_three_bundle_annotations_with_the_resolved_predicate_type() {
        let run = drive_ok(PredicateType::SpdxJson).await;
        let manifest = run.transport.pushed_referrer();

        assert_eq!(
            manifest["artifactType"].as_str(),
            Some(SIGSTORE_BUNDLE_V03),
            "signature and attestation referrers share one artifactType",
        );
        assert_eq!(manifest["layers"][0]["mediaType"].as_str(), Some(BUNDLE_V03_MEDIA_TYPE),);
        assert_eq!(
            manifest["subject"]["digest"].as_str(),
            Some(subject_digest().to_string().as_str()),
        );

        let annotations = annotations(&manifest);
        assert_eq!(annotations.len(), 3, "exactly three annotations, got: {annotations:?}");
        assert_eq!(
            annotations.get("dev.sigstore.bundle.content").map(String::as_str),
            Some("dsse-envelope"),
        );
        assert_eq!(
            annotations.get("dev.sigstore.bundle.predicateType").map(String::as_str),
            Some("https://spdx.dev/Document"),
            "the annotation carries the RESOLVED URI, not the `--type` spelling",
        );
        assert!(annotations.contains_key("org.opencontainers.image.created"));
    }

    /// D-c: the report echoes the resolved URI, so alias resolution is visible
    /// to the user rather than surprising.
    #[tokio::test]
    async fn attest_reports_the_resolved_predicate_type_uri() {
        let run = drive_ok(PredicateType::SpdxJson).await;
        let manifest = run.transport.pushed_referrer();
        let result = run.result.expect("run succeeded");

        assert_eq!(result.predicate_type, "https://spdx.dev/Document");
        assert_eq!(
            annotations(&manifest)
                .get("dev.sigstore.bundle.predicateType")
                .map(String::as_str),
            Some(result.predicate_type.as_str()),
            "the reported type and the written annotation are one value",
        );
    }

    /// The two formatters agree on a fixed instant, so whatever
    /// [`bundle_now`] resolves to — a `SOURCE_DATE_EPOCH` value or the clock —
    /// reaches the `created` annotation and the signed cosign wrapper as the
    /// same string.
    ///
    /// Deterministic on purpose. The pipeline-level sibling below drives both
    /// halves through one run, but both format at second precision and the run
    /// takes tens of milliseconds, so two independent clock reads produce the
    /// same string except across a second boundary — it cannot reliably red.
    /// This one can: change either formatter and it fails every time.
    #[test]
    fn the_annotation_and_the_wrapper_format_one_instant_identically() {
        // The same literal `manifest.rs` pins its epoch tests to.
        let fixed = chrono::DateTime::from_timestamp_secs(1_700_000_000).expect("fixed instant");
        let predicate: Box<RawValue> = serde_json::from_str(PREDICATE).expect("predicate is JSON");

        let statement = statement::build(
            "acme/tool",
            &subject_digest(),
            &PredicateType::Custom,
            &predicate,
            fixed,
        )
        .expect("statement builds");
        let wrapper: serde_json::Value = serde_json::from_str(statement.predicate.get()).expect("the wrapper is JSON");

        assert_eq!(bundle_created(fixed), "2023-11-14T22:13:20Z");
        assert_eq!(
            wrapper["Timestamp"].as_str(),
            Some(bundle_created(fixed).as_str()),
            "the signed wrapper and the `created` annotation format one instant the same way",
        );
    }

    /// The pipeline half: one `bundle_now` read feeds both consumers. Weaker
    /// than the test above by construction — see its note — but it is the only
    /// one that proves the *wiring* rather than the formatters.
    ///
    /// The `custom` type is what makes the signed half observable — it is the
    /// only predicate type whose wrapper carries a timestamp.
    #[tokio::test]
    async fn attest_stamps_one_instant_into_both_the_annotation_and_the_signed_wrapper() {
        let run = drive_ok(PredicateType::Custom).await;
        let manifest = run.transport.pushed_referrer();
        let statement = run.signer.signed_statement();

        let created = annotations(&manifest)
            .get("org.opencontainers.image.created")
            .expect("created annotation")
            .clone();
        let stamped = statement["predicate"]["Timestamp"]
            .as_str()
            .expect("the cosign wrapper carries a Timestamp");
        assert_eq!(created, stamped, "both timestamps come from one instant");

        // Go's `time.RFC3339` layout, which is what cosign formats with.
        assert_eq!(created.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ, got {created:?}");
        assert!(created.ends_with('Z'), "expected a literal Z, got {created:?}");
    }

    /// The subject bytes come from the READ host — a different machine under a
    /// `[mirrors]` entry — but their length becomes the descriptor `size`
    /// pushed to the canonical one. A poisoned length yields an attestation
    /// strict verifiers reject, so the read is bound to the resolved digest and
    /// fails closed BEFORE anything is written.
    #[tokio::test]
    async fn attest_refuses_a_subject_manifest_the_read_host_served_under_a_different_digest() {
        let transport = RecordingTransport {
            served_subject_digest: Some(Algorithm::Sha256.hash(b"a different manifest").to_string()),
            ..RecordingTransport::default()
        };
        let run = drive_attest_with(transport, PredicateType::CycloneDx, PREDICATE, false).await;

        let Err(error) = run.result else {
            panic!("attest must refuse a subject manifest served under the wrong digest");
        };
        // The kind's own Display is deliberately generic ("internal signing
        // error"); the cause rides the `source()` chain, so walk it.
        let chain = std::iter::successors(Some(&error as &dyn std::error::Error), |e| e.source())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ");
        assert!(
            chain.contains("digest mismatch"),
            "the refusal must carry the mismatch in its source chain, got: {chain}",
        );
        let calls = run.transport.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("push_")),
            "nothing may be pushed after a subject-digest mismatch, got: {calls:?}",
        );
        assert_eq!(run.acquisitions, 0, "the bind fails closed before the credential path");
    }

    // ── Index indirection: transport traffic follows the PHYSICAL registry ──

    /// The logical `ocx.sh/...` name is a pointer; the artifact lives on the
    /// physical registry the index root names. An attestation attached to the
    /// logical host is written where no verifier will ever look for it.
    #[tokio::test]
    async fn attest_pushes_the_referrer_to_the_physical_registry_not_the_logical_one() {
        let run = drive_ok(PredicateType::CycloneDx).await;
        let calls = run.transport.calls();

        for write in ["push_blob:8.8.8.8", "push_referrer_manifest:8.8.8.8"] {
            assert!(
                calls.iter().any(|call| call == write),
                "`{write}` must target the physical registry, got: {calls:?}",
            );
        }
        assert!(
            calls.iter().all(|call| call.ends_with(":8.8.8.8")),
            "no transport call may target the logical index host, got: {calls:?}",
        );
    }

    // ── Error mapping ──────────────────────────────────────────────────────

    #[test]
    fn map_client_error_preserves_referrers_unsupported() {
        let mapped = map_client_error(ClientError::ReferrersUnsupported {
            registry: "example.com".to_string(),
        });
        assert!(matches!(mapped, SignErrorKind::ReferrersUnsupported));
    }

    /// A registry fault during attach must reach the operator as the registry's
    /// own exit code, not the catch-all 1 — the same deferral contract the sign
    /// pipeline holds.
    #[test]
    fn registry_faults_keep_their_own_exit_codes_through_attest() {
        let identifier = Identifier::parse("registry.example/pkg:1.0").expect("identifier");
        let cases = [
            (
                ClientError::RegistryTransient(Box::new(std::io::Error::other("503 from registry"))),
                ExitCode::TempFail,
            ),
            (
                ClientError::Authentication(Box::new(std::io::Error::other("401 from registry"))),
                ExitCode::AuthError,
            ),
        ];
        for (client_error, expected) in cases {
            let rendered = client_error.to_string();
            let err = SignError::new(identifier.clone(), map_client_error(client_error));
            assert_eq!(classify_error(&err), expected, "client error: {rendered}");
        }
    }
}
