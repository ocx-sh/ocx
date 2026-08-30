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
//! A registry with no Referrers API gets the OCI tag-schema fallback index
//! rather than the refusal ADR S1-F specified (Amendment 10), through the same
//! `sign::referrers` helper the sign path uses.
//!
//! # `--signature-format`, and why attest has no legs
//!
//! [`SignatureFormat`] selects where the attestation is *published*, never how
//! many times it is signed. `bundle` writes the OCI 1.1 referrer above;
//! `simplesigning` writes cosign's `sha256-<hex>.att` sidecar tag, whose layer
//! is the DSSE envelope the bundle would have wrapped; `both` writes each.
//!
//! That is the one place this pipeline deliberately does **not** mirror
//! [`SignPipeline`](crate::oci::sign::SignPipeline). Sign's two legs are two
//! *independent signatures* over two different payloads — a simplesigning claim
//! is not a DSSE Statement — so each costs its own Fulcio certificate and its
//! own Rekor entry, and one leg failing must not discard the other. Attest's two
//! shapes are **one** signature at two addresses: signing twice would spend two
//! certificates on identical content and let the two publications disagree about
//! which identity attested. So the envelope is signed once and each requested
//! publication propagates its failure with `?` — a half-published attestation is
//! reported as a failure, not as a success with a hole in it.
//!
//! No `.sbom` sidecar is written. That tag is `cosign attach sbom`'s *unsigned*
//! convention, holding the raw document; the spec's §SBOM is explicit that "you
//! do not sign an SBOM, you attest it", so a signed SBOM lands on `.att` like
//! every other attestation and no signature format can produce a `.sbom`. An
//! unsigned attach asked for a sidecar is refused
//! ([`SignErrorKind::SidecarRequiresSignature`]) rather than silently given the
//! bundle shape.

use serde_json::value::RawValue;
use url::Url;

use std::collections::BTreeMap;

use super::predicate::{self, PredicateType};
use super::statement;
use crate::file_structure::StateStore;
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport};
use crate::oci::index::Index;
use crate::oci::referrer::ReferrerManifest;
use crate::oci::referrer::capability::ReferrersSupport;
use crate::oci::referrer::manifest::{bundle_annotations, bundle_created, bundle_now};
use crate::oci::referrer::media_types::{
    BUNDLE_CONTENT_DSSE, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_PAYLOAD, SIGSTORE_BUNDLE_V03,
};
use crate::oci::resolve_target::SignTarget;
use crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE;
use crate::oci::sign::pipeline::{LegDigests, resolve_platform_target};
use crate::oci::sign::referrers::{attach_referrer, map_client_error, referrers_capability};
use crate::oci::sign::simplesigning_write::{self, SidecarLayer};
use crate::oci::sign::{SignError, SignErrorKind, SignatureFormat, Signer, TokenProvider};
use crate::oci::{Algorithm, Descriptor, Digest, Identifier, OCI_IMAGE_MEDIA_TYPE, Platform, native};

/// Manifest media types accepted when fetching the per-platform target.
const ACCEPTED_MANIFEST_TYPES: &[&str] = &[
    OCI_IMAGE_MEDIA_TYPE,
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// Whether the attach publishes a signed bundle or the raw document.
///
/// Chosen by the caller from what signing material is *visible*
/// ([`DispatchingTokenProvider::has_signing_material`](crate::oci::sign::DispatchingTokenProvider::has_signing_material)),
/// never from whether acquiring one succeeded: an override token or a detected
/// ambient CI identity means [`Self::Signed`], and a failure to redeem it is a
/// hard error. Downgrading there would publish an identity-less artifact from a
/// job configured for OIDC, and the referrer would look attached either way.
///
/// [`Self::Unsigned`] is reached only when there is no signing intent at all —
/// which is where `ocx package attest` and `ocx package push --sbom` used to
/// exit 77 with `no_ambient_no_tty`. `ocx package sign` has no unsigned form and
/// still refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestMode {
    /// DSSE-sign the in-toto Statement and publish it as a Sigstore bundle v0.3
    /// referrer.
    Signed,
    /// Publish the predicate document itself as the referrer payload, typed by
    /// its own SBOM media type. No Fulcio, no Rekor, no DSSE.
    Unsigned,
}

/// Context passed into [`AttestPipeline::run`] — all external dependencies.
pub struct AttestContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Narrowing selector, when one was requested — see
    /// [`SignContext::platform`](crate::oci::sign::SignContext::platform).
    /// `None` acts on whatever the reference resolved to.
    pub platform: Option<&'a Platform>,
    /// Whether to sign. Both dependencies below are read in
    /// [`AttestMode::Signed`] only.
    pub mode: AttestMode,
    /// Which cosign wire shape(s) to publish the attestation in — the referrer
    /// bundle, the `sha256-<hex>.att` sidecar, or both. See the module doc for
    /// why this multiplies publications and never signatures.
    pub format: SignatureFormat,
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
    /// A resolution the caller already performed for `identifier` — see
    /// [`SignContext::resolved`](crate::oci::sign::SignContext::resolved),
    /// which this mirrors so the `--tags` sweep costs one manifest fetch per
    /// tag on both verbs (#373).
    pub resolved: Option<&'a (Digest, crate::oci::Manifest)>,
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
    /// Where the `bundle` shape landed: the payload blob (the Sigstore bundle
    /// under [`AttestMode::Signed`], the SBOM document itself under
    /// [`AttestMode::Unsigned`]) and the OCI referrer manifest above it.
    ///
    /// `None` under `--signature-format simplesigning`, which publishes the
    /// sidecar alone. At least one of this and [`Self::sidecar`] is always
    /// `Some` — `SignatureFormat` has no variant that writes nothing.
    pub referrer: Option<LegDigests>,
    /// Where the `simplesigning` shape landed: the DSSE envelope blob and the
    /// `sha256-<hex>.att` sidecar manifest above it. `None` unless the sidecar
    /// was asked for.
    pub sidecar: Option<LegDigests>,
    /// Whether the referrer carries a signature. `false` means the document was
    /// attached as-is, with no identity behind it.
    pub signed: bool,
    /// Cert SAN (identity) that signed the Statement — the OIDC subject.
    /// `None` on an unsigned attach, where no certificate was ever issued.
    pub certificate_identity: Option<String>,
    /// Cert issuer (`--certificate-oidc-issuer` comparand) — the OIDC issuer.
    /// `None` on an unsigned attach.
    pub certificate_oidc_issuer: Option<String>,
    /// Which key model produced the signature; `None` on an unsigned attach.
    pub key_backend: Option<crate::oci::sign::KeyBackendKind>,
    /// The signing key's cosign hint, in key mode only.
    pub public_key_hint: Option<String>,
    /// The Rekor log index, when a transparency record was created.
    ///
    /// Reported rather than inferred: under a key `--rekor-upload` is opt-in,
    /// so its absence is a legal outcome the operator must be able to see.
    pub transparency_log_index: Option<u64>,
}

/// The referrer's single payload layer: the bytes, their digest, what types
/// them, and the `artifactType` the manifest above them declares.
///
/// Grouped rather than passed as positionals so [`AttestPipeline::push_referrer`]
/// stays under the argument limit and so the digest cannot drift from the bytes
/// it was computed over — the digest is the blob's registry address, and a
/// swapped pair of adjacent arguments would type-check.
///
/// `artifact_type` belongs here rather than beside it because the two are one
/// decision: a Sigstore bundle layer under a `sigstore.bundle.v0.3` referrer, an
/// SBOM document layer under its own type. Splitting them is how a signed
/// bundle ends up advertised as an unsigned SBOM.
struct ReferrerPayload {
    bytes: Vec<u8>,
    digest: Digest,
    media_type: &'static str,
    artifact_type: &'static str,
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
        // 1a. The unsigned floor, ahead of the provenance one because in
        //     unsigned mode it is the more specific answer: without a DSSE
        //     envelope the referrer's `artifactType` is the only place the
        //     document's type can be recorded, so a type with no SBOM media
        //     type cannot be attached at all — at any provenance version.
        //     Pure function of `--type` and the mode, so it costs no network
        //     and, like the floor below, no credential.
        if ctx.mode == AttestMode::Unsigned && predicate::sbom_artifact_type(ctx.predicate_type).is_none() {
            return Err(SignErrorKind::UnsignedTypeUnsupported { predicate_type });
        }
        // 1b. The sidecar floor. A `sha256-<hex>.att` layer IS a DSSE envelope,
        //     so an unsigned attach has nothing to put in one — and quietly
        //     publishing the bundle shape instead would make
        //     `--signature-format` a flag that did something other than what it
        //     says. Pure function of the mode and the flag, so like 1a it costs
        //     no network and no credential.
        if ctx.mode == AttestMode::Unsigned && ctx.format.writes_simplesigning() {
            return Err(SignErrorKind::SidecarRequiresSignature { format: ctx.format });
        }
        if predicate::is_provenance_below_v1(ctx.predicate_type) {
            return Err(SignErrorKind::ProvenanceVersionUnsupported {
                resolved: predicate_type,
            });
        }

        // 2. SSRF floor for the trust services (CWE-918). The CLI boundary
        //    validated these URLs as *strings*; this is where we find out where
        //    they actually resolve, before anything dials them. A signed attach
        //    always reaches Fulcio and Rekor, so both are guarded.
        //
        //    Skipped in unsigned mode, and that is not a relaxation: the
        //    unsigned tail dials neither service, so resolving them would make
        //    an attach that touches no Sigstore endpoint depend on DNS for two
        //    hosts it will never open a socket to. The registry-side dial guard
        //    below is unconditional.
        //    Within signed mode it narrows again, per endpoint the signer will
        //    actually dial: key mode reaches no Fulcio and, by default, no
        //    Rekor.
        if ctx.mode == AttestMode::Signed {
            let trusted = ctx.index.trusted_hosts_for(ctx.identifier.registry()).to_vec();
            crate::oci::sign::pipeline::guard_dialed_endpoints(&trusted, ctx.signer, ctx.fulcio_url, ctx.rekor_url)
                .await?;
        }

        let transport = client.transport();
        // 3. Resolve the target manifest under the `--platform` optionality
        //    rule, through the one module that owns it. This digest is the
        //    subject — never one derived from a keep tag, which
        //    `--no-keep-tag` may have suppressed (D-f).
        let SignTarget { subject_digest, .. } =
            resolve_platform_target(ctx.index, ctx.identifier, ctx.platform, ctx.resolved).await?;
        let resolved = ctx.identifier.clone_with_digest(subject_digest.clone());
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
        //
        // The verdict decides whether the tag-schema fallback index is written
        // alongside the manifest; it no longer decides whether attaching may
        // happen at all.
        let referrers_support =
            referrers_capability(transport, &write_image, &subject_digest, ctx.state, ctx.no_cache).await?;

        // 5. Build the subject descriptor both modes attach to. Its `size` is
        //    the length of the bytes bound to the resolved digest above.
        let subject_descriptor = Descriptor {
            media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: subject_digest.to_string(),
            size: subject_bytes.len() as i64,
            ..Descriptor::default()
        };

        // 6. Publish. The two modes diverge here and only here: what the
        //    referrer's one layer holds, what types it, and whether the run
        //    touches a credential at all.
        match ctx.mode {
            AttestMode::Signed => {
                // Acquire the OIDC token. A failure is terminal: the mode was
                // chosen because signing material was visible, and falling back
                // to an unsigned attach would publish an identity-less artifact
                // from a job configured for OIDC.
                let token = match ctx.signer.requires_identity_token() {
                    true => Some(ctx.token_provider.acquire("sigstore").await?),
                    false => None,
                };

                // Build the in-toto Statement and sign it as a DSSE envelope.
                // One instant serves both the signed cosign wrapper and the
                // `created` annotation below, so a `SOURCE_DATE_EPOCH` run
                // stamps the same value in both places.
                let now = bundle_now();
                let statement = statement::build(
                    physical.repository(),
                    &subject_digest,
                    ctx.predicate_type,
                    ctx.predicate,
                    now,
                )?;
                let statement_bytes =
                    serde_json::to_vec(&statement).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
                let bundle = ctx
                    .signer
                    .sign_dsse(&statement_bytes, token.as_ref(), ctx.fulcio_url, ctx.rekor_url)
                    .await?;

                // One signature, published wherever `--signature-format` asked
                // for it. Each leg propagates with `?` rather than being
                // collected: see the module doc for why attest has no legs.
                let referrer = match ctx.format.writes_bundle() {
                    true => {
                        // cosign parity (ADR D1): an attestation referrer carries
                        // the same `artifactType` a signature does and is told
                        // apart by `content: dsse-envelope`, with the RESOLVED
                        // predicateType as the third annotation. The set is a
                        // one-way door — the manifest's SHA-256 *is* the
                        // referrer's registry address.
                        let annotations =
                            bundle_annotations(&bundle_created(now), BUNDLE_CONTENT_DSSE, &predicate_type);
                        let payload = ReferrerPayload {
                            digest: bundle.digest.clone(),
                            bytes: bundle.bytes,
                            media_type: BUNDLE_V03_MEDIA_TYPE,
                            artifact_type: SIGSTORE_BUNDLE_V03,
                        };
                        let (referrer_digest, _descriptor) = Self::push_referrer(
                            transport,
                            &write_image,
                            &subject_digest,
                            subject_descriptor,
                            payload,
                            Some(annotations),
                            referrers_support,
                        )
                        .await?;
                        Some(LegDigests {
                            payload_digest: bundle.digest,
                            manifest_digest: referrer_digest,
                        })
                    }
                    false => None,
                };

                let sidecar = match ctx.format.writes_simplesigning() {
                    true => {
                        // The bare DSSE envelope, not the bundle: cosign's
                        // `.att` tag predates bundles and its layer has always
                        // been the envelope itself, typed
                        // `application/vnd.dsse.envelope.v1+json`. The
                        // verification material the bundle carries structurally
                        // travels in layer annotations here instead.
                        let payload_digest = Algorithm::Sha256.hash(&bundle.envelope_json);
                        let layer = SidecarLayer::attestation(
                            bundle.envelope_json,
                            bundle.certificate_pem.as_deref(),
                            bundle.rekor_bundle.as_deref(),
                        );
                        // `write_image`, never the mirrored read reference: the
                        // append PUTs a tag through whatever host it is handed,
                        // and an attestation written to a read-only mirror is
                        // one the canonical verifier never looks at.
                        let manifest_digest =
                            simplesigning_write::append_layer(transport, &write_image, &subject_digest, &layer).await?;
                        Some(LegDigests {
                            payload_digest,
                            manifest_digest,
                        })
                    }
                    false => None,
                };

                Ok(AttestResult {
                    subject_digest,
                    predicate_type,
                    referrer,
                    sidecar,
                    signed: true,
                    certificate_identity: Some(bundle.certificate_identity),
                    certificate_oidc_issuer: Some(bundle.certificate_oidc_issuer),
                    key_backend: Some(bundle.key_backend),
                    public_key_hint: bundle.public_key_hint,
                    transparency_log_index: bundle.transparency_log_index,
                })
            }
            AttestMode::Unsigned => {
                // `Some` by construction — step 1a returned early otherwise,
                // and nothing since could have changed `--type`. Returned
                // rather than asserted: a panic in library code is never the
                // better half of that trade.
                let artifact_type = predicate::sbom_artifact_type(ctx.predicate_type).ok_or_else(|| {
                    SignErrorKind::UnsignedTypeUnsupported {
                        predicate_type: predicate_type.clone(),
                    }
                })?;

                // The document travels verbatim, exactly as the signed path
                // splices it into the Statement: whatever whitespace, key order
                // and number spelling the predicate file held is what a reader
                // gets back, and its SHA-256 is what addresses the blob.
                let document = ctx.predicate.get().as_bytes().to_vec();
                let payload = ReferrerPayload {
                    digest: Algorithm::Sha256.hash(&document),
                    bytes: document,
                    media_type: artifact_type,
                    artifact_type,
                };
                let payload_digest = payload.digest.clone();
                // No annotations at all. The three `dev.sigstore.bundle.*` keys
                // describe a bundle this referrer does not carry, and writing
                // them would make an unsigned document look like a signed one
                // in a listing — the exact confusion the artifactType split
                // exists to prevent. `cosign attach sbom` writes none either.
                let (referrer_digest, _descriptor) = Self::push_referrer(
                    transport,
                    &write_image,
                    &subject_digest,
                    subject_descriptor,
                    payload,
                    None,
                    referrers_support,
                )
                .await?;

                Ok(AttestResult {
                    subject_digest,
                    predicate_type,
                    referrer: Some(LegDigests {
                        payload_digest,
                        manifest_digest: referrer_digest,
                    }),
                    // Unreachable by construction: step 1b refuses a sidecar
                    // request before this arm is entered, because an unsigned
                    // attach has no DSSE envelope to put in one.
                    sidecar: None,
                    signed: false,
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                    key_backend: None,
                    public_key_hint: None,
                    transparency_log_index: None,
                })
            }
        }
    }

    /// Push the referrer's blobs, then its manifest, and return the manifest's
    /// digest and descriptor.
    ///
    /// Both modes land here: a spec-strict registry (zot) rejects the manifest
    /// with `MANIFEST_INVALID` unless the OCI empty-config blob and the payload
    /// blob are both already present, so both are pushed before the manifest
    /// PUT. What differs between the modes is only what the payload *is*.
    async fn push_referrer(
        transport: &dyn OciTransport,
        write_image: &native::Reference,
        subject_digest: &Digest,
        subject: Descriptor,
        payload: ReferrerPayload,
        annotations: Option<BTreeMap<String, String>>,
        support: ReferrersSupport,
    ) -> Result<(Digest, Descriptor), SignErrorKind> {
        let no_progress: std::sync::Arc<dyn Fn(u64) + Send + Sync> = std::sync::Arc::new(|_| ());
        let empty_config_digest =
            Digest::try_from(EMPTY_CONFIG_DIGEST).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        transport
            .push_blob(
                write_image,
                EMPTY_CONFIG_PAYLOAD.to_vec(),
                &empty_config_digest,
                no_progress.clone(),
            )
            .await
            .map_err(map_client_error)?;
        let payload_descriptor = Descriptor {
            media_type: payload.media_type.to_string(),
            digest: payload.digest.to_string(),
            size: payload.bytes.len() as i64,
            ..Descriptor::default()
        };
        transport
            .push_blob(write_image, payload.bytes, &payload.digest, no_progress)
            .await
            .map_err(map_client_error)?;

        let manifest = ReferrerManifest::build(subject, payload.artifact_type, payload_descriptor, annotations);
        let manifest_bytes = manifest.to_canonical_json()?;
        // `write_image` is `transport_write_reference`'s: the fallback append
        // PUTs a tag through whatever host it is handed, and an attestation
        // written to a mirror is one the canonical verifier never looks at
        // (CWE-345/367, `oci/client.rs:164-181`).
        attach_referrer(transport, write_image, subject_digest, &manifest_bytes, support).await
    }

    // The Unsupported verdict no longer refuses the operation: the OCI referrers
    // tag-schema fallback (`list_referrers_with_fallback` /
    // `append_referrer_fallback_index`) serves a registry without the Referrers
    // API. See `adr_oci_referrers_signing_v1.md`, Amendment 10 — the fallback
    // index is a mutable tag anyone with push access authors, and the residual
    // attack surface that reverses S1-F is recorded there.
    //
    // This file carried its own byte-identical copy of the sign pipeline's gate.
    // Deleting only the sign one would have left `ocx package attest` refused on
    // exactly the registries `ocx package sign` had just started working on, with
    // nothing in the build to notice. Both now read the verdict through
    // `sign::referrers::referrers_capability` and write through
    // `sign::referrers::attach_referrer`.
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
            _: crate::oci::index::IndexOperation,
        ) -> crate::Result<Option<(Digest, crate::oci::Manifest)>> {
            Ok(Some((
                subject_digest(),
                crate::oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )))
        }

        async fn fetch_manifest_digest(
            &self,
            _: &Identifier,
            _: crate::oci::index::IndexOperation,
        ) -> crate::Result<Option<Digest>> {
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
        /// When true, `list_referrers` answers `ReferrersUnsupported` — the
        /// `registry:2` shape, where the tag-schema fallback is the only way an
        /// attestation becomes discoverable.
        referrers_unsupported: bool,
        /// Tag-addressed manifest store, so the fallback index's
        /// read-append-write-read-back loop runs for real.
        manifests: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
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

        /// The bytes stored at `reference`, if any — how a test reads back the
        /// fallback index this transport was asked to hold.
        fn stored(&self, reference: &str) -> Option<Vec<u8>> {
            self.manifests
                .lock()
                .expect("manifest store lock")
                .get(reference)
                .cloned()
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
            // The fallback index is tag-addressed, so its read must be served
            // from the store this transport writes into; anything else is the
            // subject-manifest read the pipeline makes first.
            if let Some(bytes) = self.stored(&image.whole()) {
                let digest = Algorithm::Sha256.hash(&bytes).to_string();
                return Ok((bytes, digest));
            }
            if image.tag().is_some_and(|tag| tag.starts_with("sha256-")) {
                return Err(ClientError::ManifestNotFound(image.whole()));
            }
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
            image: &native::Reference,
            bytes: Vec<u8>,
            _: &str,
        ) -> std::result::Result<String, ClientError> {
            // Records the whole reference, not just the registry: the fallback
            // index is identified by its TAG, and a recorder that drops the tag
            // cannot tell a fallback write from any other manifest PUT.
            self.calls
                .lock()
                .expect("recorder lock")
                .push(format!("push_manifest_raw:{}", image.whole()));
            self.manifests
                .lock()
                .expect("manifest store lock")
                .insert(image.whole(), bytes);
            Ok(image.whole())
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

        async fn push_blob_from_path(
            &self,
            image: &native::Reference,
            path: &std::path::Path,
            digest: &Digest,
            on_progress: Arc<dyn Fn(u64) + Send + Sync>,
        ) -> std::result::Result<String, ClientError> {
            crate::oci::client::push_blob_buffered(self, image, path, digest, on_progress).await
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
            // `artifactType` and the annotations are read back out of the bytes
            // just pushed, exactly as `NativeTransport` does. A double leaving
            // them `None` would pass a test asserting they survive the fallback
            // append while the real transport's value went unexercised.
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

        async fn list_referrers(
            &self,
            image: &native::Reference,
            _: &Digest,
            _: Option<&str>,
        ) -> std::result::Result<Vec<Descriptor>, ClientError> {
            // A successful (empty) listing is what the capability probe reads
            // as "this registry supports the Referrers API"; the refusal is
            // what it reads as `Unsupported`.
            self.record("list_referrers", image);
            if self.referrers_unsupported {
                return Err(ClientError::ReferrersUnsupported {
                    registry: image.resolve_registry().to_string(),
                });
            }
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
    /// was never entered — not merely that no request was sent. `fails` makes
    /// it a CI whose OIDC configuration is broken: detection said an identity
    /// was there, redeeming it does not work.
    #[derive(Clone, Default)]
    struct CountingTokenProvider {
        acquisitions: Arc<Mutex<usize>>,
        fails: bool,
    }

    #[async_trait::async_trait]
    impl TokenProvider for CountingTokenProvider {
        async fn acquire(&self, _: &str) -> Result<OidcToken, SignErrorKind> {
            *self.acquisitions.lock().expect("counter lock") += 1;
            if self.fails {
                return Err(SignErrorKind::OidcTokenRejected);
            }
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
            serde_json::from_slice(&self.signed_bytes()).expect("statement is JSON")
        }

        /// The one Statement a successful run signs, verbatim — the bytes the
        /// returned DSSE envelope carries as its `payload`.
        fn signed_bytes(&self) -> Vec<u8> {
            let signed = self.signed.lock().expect("recorder lock").clone();
            let [bytes] = signed.as_slice() else {
                panic!("expected exactly one sign_dsse call, got {}", signed.len());
            };
            bytes.clone()
        }
    }

    #[async_trait::async_trait]
    impl Signer for RecordingSigner {
        async fn sign_blob(
            &self,
            _: &[u8],
            _: Option<&OidcToken>,
            _: &Url,
            _: &Url,
        ) -> Result<crate::oci::sign::SignedBlob, SignErrorKind> {
            unimplemented!("the attest pipeline signs statements, never bare blobs")
        }

        async fn sign_dsse(
            &self,
            statement_bytes: &[u8],
            _: Option<&OidcToken>,
            _: &Url,
            _: &Url,
        ) -> Result<SignedBundle, SignErrorKind> {
            self.signed
                .lock()
                .expect("recorder lock")
                .push(statement_bytes.to_vec());
            let bytes = br#"{"mediaType":"test-dsse-bundle"}"#.to_vec();
            let digest = Algorithm::Sha256.hash(&bytes);
            // A recognisable stand-in for the DSSE envelope the real signers
            // return: the `.att` sidecar layer is these bytes verbatim, so a
            // test can address the layer by hashing them.
            Ok(SignedBundle {
                key_backend: crate::oci::sign::KeyBackendKind::Keyless,
                public_key_hint: None,
                transparency_log_index: Some(1),
                bytes,
                digest,
                certificate_identity: "me@example.com".to_string(),
                certificate_oidc_issuer: "https://issuer.example".to_string(),
                envelope_json: test_envelope(statement_bytes),
                certificate_pem: Some(TEST_CERTIFICATE_PEM.to_string()),
                rekor_bundle: Some(TEST_REKOR_BUNDLE.to_string()),
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
        drive_attest_with(
            RecordingTransport::default(),
            AttestMode::Signed,
            predicate_type,
            predicate,
            offline,
            false,
            SignatureFormat::Bundle,
        )
        .await
    }

    /// `drive_attest` with the wire shape under test and nothing else varied.
    async fn drive_format(format: SignatureFormat) -> Run {
        let run = drive_attest_with(
            RecordingTransport::default(),
            AttestMode::Signed,
            PredicateType::CycloneDx,
            PREDICATE,
            false,
            false,
            format,
        )
        .await;
        if let Err(error) = &run.result {
            panic!("attest must complete for --signature-format {format}: {error}");
        }
        run
    }

    /// `drive_attest` on the unsigned tail: no signer, no token, no Sigstore.
    async fn drive_unsigned(predicate_type: PredicateType, predicate: &str) -> Run {
        drive_attest_with(
            RecordingTransport::default(),
            AttestMode::Unsigned,
            predicate_type,
            predicate,
            false,
            false,
            SignatureFormat::Bundle,
        )
        .await
    }

    /// `drive_attest` with the transport, the mode and the credential outcome
    /// injected, so a test can drive a misbehaving transport, the unsigned
    /// tail, or a signed run whose identity cannot be redeemed.
    #[allow(clippy::too_many_arguments)]
    async fn drive_attest_with(
        transport: RecordingTransport,
        mode: AttestMode,
        predicate_type: PredicateType,
        predicate: &str,
        offline: bool,
        token_fails: bool,
        format: SignatureFormat,
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
        // `None`, not `Some(Platform::any())`: `IndirectingIndex` resolves to a
        // bare image manifest, and under the narrowing rule a platform against
        // a bare manifest is `TargetNotAnIndex`. Attesting whatever resolved is
        // what this fixture is about.
        let signer = RecordingSigner::default();
        let token_provider = CountingTokenProvider {
            fails: token_fails,
            ..CountingTokenProvider::default()
        };
        let predicate: Box<RawValue> = serde_json::from_str(predicate).expect("predicate is JSON");

        let result = AttestPipeline::run(
            &client,
            AttestContext {
                identifier: &logical,
                platform: None,
                mode,
                format,
                signer: &signer,
                token_provider: &token_provider,
                predicate_type: &predicate_type,
                predicate: &predicate,
                no_cache: true,
                offline,
                index: &index,
                resolved: None,
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

    /// The DSSE envelope `RecordingSigner` hands back — what an `.att` layer
    /// must hold verbatim.
    ///
    /// Its `payload` is base64 of the statement, exactly as a real envelope's
    /// is, so two runs over two different predicates produce two different
    /// envelopes. That is what makes the append test measure appending rather
    /// than measuring the deduplication a byte-identical fake would trigger.
    fn test_envelope(statement_bytes: &[u8]) -> Vec<u8> {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::STANDARD.encode(statement_bytes);
        format!(
            r#"{{"payloadType":"application/vnd.in-toto+json","payload":"{payload}","signatures":[{{"sig":"c2ln"}}]}}"#
        )
        .into_bytes()
    }

    /// Stand-ins for the two annotations an `.att` layer carries.
    const TEST_CERTIFICATE_PEM: &str = "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----\n";
    const TEST_REKOR_BUNDLE: &str = r#"{"SignedEntryTimestamp":"c2V0","Payload":{"logIndex":1}}"#;

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
    /// keep tag, the logical reference — attests to an artifact the
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
        let run = drive_attest_with(
            transport,
            AttestMode::Signed,
            PredicateType::CycloneDx,
            PREDICATE,
            false,
            false,
            SignatureFormat::Bundle,
        )
        .await;

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

    // ── C-009: the referrers fallback, wired on the attest side too ──

    /// The referrers fallback tag for `subject_digest()`.
    fn fallback_tag_reference() -> String {
        format!("8.8.8.8/acme/tool:sha256-{}", subject_digest().hex())
    }

    /// **The attest half of C-009.** `attest/pipeline.rs` carried its own
    /// byte-identical copy of the sign gate, so wiring only `sign` would have
    /// left `ocx package attest` refused with exit 84 on exactly the registries
    /// `ocx package sign` had just started working on — with nothing in the
    /// build to notice, since `-D dead-code` cannot fire on a `pub` trait
    /// method in a library crate.
    #[tokio::test]
    async fn attest_writes_the_fallback_index_on_a_registry_without_the_referrers_api() {
        let transport = RecordingTransport {
            referrers_unsupported: true,
            ..RecordingTransport::default()
        };
        let run = drive_attest_with(
            transport.clone(),
            AttestMode::Signed,
            PredicateType::CycloneDx,
            PREDICATE,
            false,
            false,
            SignatureFormat::Bundle,
        )
        .await;
        run.result
            .expect("attest must succeed on a registry without the Referrers API");

        let tag_reference = fallback_tag_reference();
        let calls = run.transport.calls();
        assert!(
            calls.contains(&format!("push_manifest_raw:{tag_reference}")),
            "the fallback index tag must be written, got: {calls:?}",
        );

        // Spec step 5 is a MUST, and the half sigstore/cosign#4641 gets wrong.
        let stored = transport.stored(&tag_reference).expect("the fallback index was stored");
        let index: crate::oci::ImageIndex = serde_json::from_slice(&stored).expect("fallback index parses");
        assert_eq!(index.manifests.len(), 1, "one referrer descriptor appended");
        assert_eq!(
            index.manifests[0].artifact_type.as_deref(),
            Some(SIGSTORE_BUNDLE_V03),
            "artifactType survives the append",
        );
    }

    /// The unsigned `attach sbom` tail reaches the same seam, and must not be
    /// left behind on a fallback registry either.
    #[tokio::test]
    async fn an_unsigned_sbom_attach_also_writes_the_fallback_index() {
        let transport = RecordingTransport {
            referrers_unsupported: true,
            ..RecordingTransport::default()
        };
        let run = drive_attest_with(
            transport.clone(),
            AttestMode::Unsigned,
            PredicateType::CycloneDx,
            PREDICATE,
            false,
            false,
            SignatureFormat::Bundle,
        )
        .await;
        run.result
            .expect("an unsigned attach must succeed on a fallback registry");
        assert!(
            transport.stored(&fallback_tag_reference()).is_some(),
            "the unsigned attach path must write the fallback index too, got: {:?}",
            run.transport.calls(),
        );
    }

    /// The other half: a registry serving the Referrers API computes its own
    /// listing, so nothing writes the attacker-authorable tag beside it.
    #[tokio::test]
    async fn attest_writes_no_fallback_index_on_a_registry_with_the_referrers_api() {
        let run = drive_ok(PredicateType::CycloneDx).await;
        let calls = run.transport.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("push_manifest_raw:")),
            "a supported registry needs no fallback tag, got: {calls:?}",
        );
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

    // ── The unsigned tail ──────────────────────────────────────────────────

    /// The wire shape of an unsigned attach: the SBOM document *is* the
    /// referrer payload, and the `artifactType` is what says so. A signed
    /// attach records the document's type in the DSSE `predicateType` inside a
    /// bundle; with no bundle there is nowhere else to put it, so the
    /// artifactType and the layer mediaType carry it and must agree.
    #[tokio::test]
    async fn an_unsigned_attach_publishes_the_document_typed_by_its_own_media_type() {
        let run = drive_unsigned(PredicateType::CycloneDx, PREDICATE).await;
        let result = run.result.expect("an unsigned attach needs no credential");
        let manifest = run.transport.pushed_referrer();

        assert_eq!(
            manifest["artifactType"].as_str(),
            Some("application/vnd.cyclonedx+json"),
            "an unsigned SBOM referrer is typed by the document, not by a bundle",
        );
        assert_eq!(
            manifest["layers"][0]["mediaType"].as_str(),
            Some("application/vnd.cyclonedx+json"),
            "the payload layer carries the same type the referrer advertises",
        );
        assert_eq!(
            manifest["layers"][0]["digest"].as_str(),
            Some(Algorithm::Sha256.hash(PREDICATE.as_bytes()).to_string().as_str()),
            "the layer digest addresses the verbatim predicate bytes",
        );
        assert_eq!(
            manifest["subject"]["digest"].as_str(),
            Some(subject_digest().to_string().as_str()),
        );
        assert!(!result.signed, "nothing signed this");
        assert_eq!(result.certificate_identity, None, "no certificate was ever issued");
        assert_eq!(result.certificate_oidc_issuer, None);
        assert_eq!(result.predicate_type, "https://cyclonedx.org/bom");
    }

    /// No `dev.sigstore.*` keys, and no `annotations` object at all.
    ///
    /// The three bundle annotations describe a bundle this referrer does not
    /// carry, so writing them would make an unsigned document indistinguishable
    /// from a signed one in a listing — which is the confusion the artifactType
    /// split exists to prevent. Absence is asserted as absence of the *key*,
    /// because the manifest's SHA-256 is the referrer's registry address and
    /// `"annotations": null` is different bytes.
    #[tokio::test]
    async fn an_unsigned_referrer_carries_no_sigstore_annotations_at_all() {
        let run = drive_unsigned(PredicateType::CycloneDx, PREDICATE).await;
        run.result.expect("run succeeded");
        let manifest = run.transport.pushed_referrer();

        assert!(
            !manifest
                .as_object()
                .expect("manifest is an object")
                .contains_key("annotations"),
            "an unsigned referrer writes no annotations key: {manifest}",
        );
        // The positive control, so this cannot pass by annotations having been
        // dropped everywhere: the signed tail still writes all three.
        let signed = drive_ok(PredicateType::CycloneDx).await;
        assert_eq!(annotations(&signed.transport.pushed_referrer()).len(), 3);
    }

    /// An unsigned attach touches neither the credential path nor the signer.
    /// It is not merely that no Sigstore request is sent — the token is never
    /// resolved, so a run with no identity available cannot fail on one.
    #[tokio::test]
    async fn an_unsigned_attach_resolves_no_token_and_signs_nothing() {
        let run = drive_unsigned(PredicateType::CycloneDx, PREDICATE).await;
        run.result.expect("run succeeded");

        assert_eq!(run.acquisitions, 0, "the unsigned tail must not resolve a token");
        assert!(
            run.signer.signed.lock().expect("recorder lock").is_empty(),
            "the unsigned tail must not sign anything",
        );
        // The blobs still land: a spec-strict registry rejects the manifest
        // unless the empty config and the payload are both already present.
        let pushes = run
            .transport
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("push_blob"))
            .count();
        assert_eq!(pushes, 2, "the empty config and the document are both pushed");
    }

    /// The predicate travels verbatim into the blob, exactly as the signed path
    /// splices it into the Statement. `--output` on the read side promises the
    /// bytes back unchanged, and a re-serialization here would break that
    /// silently — the digest would still be self-consistent.
    #[tokio::test]
    async fn an_unsigned_attach_pushes_the_predicate_bytes_verbatim() {
        // Spelled so a `Value` round-trip is observable: a trailing-zero float
        // and a non-alphabetical key order.
        const SPELLED: &str = r#"{"zeta":1,"alpha":1.50}"#;
        let run = drive_unsigned(PredicateType::CycloneDx, SPELLED).await;
        let result = run.result.expect("run succeeded");

        let referrer = result.referrer.expect("an unsigned attach publishes a referrer");
        assert_eq!(
            referrer.payload_digest,
            Algorithm::Sha256.hash(SPELLED.as_bytes()),
            "the payload digest must address the caller's own bytes, not a re-serialization",
        );
    }

    /// `spdx` and `spdxjson` share one predicateType URI and do not share a
    /// serialization, so the unsigned map is the one dispatch that reads the
    /// variant. Both rows are needed: a map that answered `application/spdx+json`
    /// for everything would pass a single-row test.
    #[tokio::test]
    async fn the_two_spdx_spellings_attach_under_different_media_types() {
        let cases = [
            (PredicateType::Spdx, "text/spdx"),
            (PredicateType::SpdxJson, "application/spdx+json"),
            (PredicateType::CycloneDx, "application/vnd.cyclonedx+json"),
        ];
        for (predicate_type, expected) in cases {
            let run = drive_unsigned(predicate_type.clone(), PREDICATE).await;
            run.result.expect("run succeeded");
            let manifest = run.transport.pushed_referrer();
            assert_eq!(
                manifest["artifactType"].as_str(),
                Some(expected),
                "{predicate_type:?} must attach as {expected}",
            );
        }
    }

    /// The unsigned floor: a predicate type with no SBOM media type has nowhere
    /// to record what it is, so it cannot be attached unsigned at all. 64, not
    /// 65 — the offending value came from the invocation, and the fix is to
    /// supply an identity rather than a different file.
    #[tokio::test]
    async fn an_unsigned_attach_refuses_every_predicate_type_that_is_not_an_sbom() {
        let refused = [
            PredicateType::SlsaProvenance,
            PredicateType::SlsaProvenance1,
            PredicateType::Custom,
            PredicateType::Vuln,
            PredicateType::Uri("https://example.test/whatever".to_string()),
        ];
        for predicate_type in refused {
            let run = drive_unsigned(predicate_type.clone(), PREDICATE).await;
            let Err(error) = run.result else {
                panic!("{predicate_type:?} has no SBOM media type and must be refused");
            };
            let SignErrorKind::UnsignedTypeUnsupported { predicate_type: named } = &error.kind else {
                panic!("expected the unsigned floor for {predicate_type:?}, got: {error}");
            };
            assert_eq!(named, predicate_type.uri(), "the refusal names the resolved type");
            assert_eq!(classify_error(&error), ExitCode::UsageError, "for {predicate_type:?}");
            assert!(
                error.kind.to_string().contains("supply an OIDC identity"),
                "the refusal must name the fix, got: {}",
                error.kind,
            );
            assert!(
                run.transport.calls().is_empty(),
                "the floor must refuse before any traffic, got: {:?}",
                run.transport.calls(),
            );
        }
    }

    /// The converse half, and the reason the floor cannot simply refuse
    /// everything: every SBOM spelling passes it.
    #[tokio::test]
    async fn an_unsigned_attach_accepts_every_sbom_predicate_type() {
        for predicate_type in [PredicateType::CycloneDx, PredicateType::Spdx, PredicateType::SpdxJson] {
            let run = drive_unsigned(predicate_type.clone(), PREDICATE).await;
            assert!(
                run.result.is_ok(),
                "{predicate_type:?} is an SBOM type and must attach, got: {:?}",
                run.result.err().map(|e| e.to_string()),
            );
        }
    }

    /// The floor is scoped to the unsigned mode: a signed attach still accepts
    /// the same non-SBOM types it always did. Without this the floor could have
    /// been written mode-blind and both this file's provenance tests and the
    /// one above would still pass.
    #[tokio::test]
    async fn the_unsigned_floor_does_not_reach_a_signed_attach() {
        for predicate_type in [PredicateType::SlsaProvenance1, PredicateType::Custom] {
            let run = drive_attest(predicate_type.clone(), PREDICATE, false).await;
            assert!(
                run.result.is_ok(),
                "{predicate_type:?} must still attach when signed, got: {:?}",
                run.result.err().map(|e| e.to_string()),
            );
        }
    }

    /// No silent downgrade. A run that chose the signed mode did so because a
    /// signing identity was visible; if redeeming it fails, the attach fails.
    /// Falling back to unsigned here would publish an identity-less artifact
    /// from a job configured for OIDC, and the referrer would look attached
    /// either way — the failure would surface only when someone tried to verify
    /// it, long after the bytes were immutable.
    #[tokio::test]
    async fn a_signed_attach_whose_identity_cannot_be_redeemed_publishes_nothing() {
        let run = drive_attest_with(
            RecordingTransport::default(),
            AttestMode::Signed,
            PredicateType::CycloneDx,
            PREDICATE,
            false,
            true,
            SignatureFormat::Bundle,
        )
        .await;

        let Err(error) = run.result else {
            panic!("an unredeemable identity must fail the attach, never downgrade it");
        };
        assert!(
            matches!(error.kind, SignErrorKind::OidcTokenRejected),
            "the credential failure must surface as itself, got: {error}",
        );
        assert_eq!(run.acquisitions, 1, "the run did try to redeem the identity");
        let calls = run.transport.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("push_")),
            "nothing may be published after the identity failed, got: {calls:?}",
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

    mod signature_format_tests {
        //! WP4: `--signature-format` on `ocx package attest`.
        //!
        //! Every row drives the real pipeline against the recording transport and
        //! reads what landed at the registry, because the contract is *what was
        //! written where* — a `SignatureFormat` echoed back into the result would
        //! prove only that the field was carried.

        use super::*;
        use crate::cli::{ExitCode, classify_error};
        use crate::oci::ImageManifest;
        use crate::oci::referrer::media_types::{
            ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE, ANNOTATION_COSIGN_SIGNATURE,
            DSSE_ENVELOPE_MEDIA_TYPE,
        };

        /// The cosign sidecar tag an attestation over the fixture subject lands on.
        ///
        /// Spelled out rather than derived from `sidecar_tag`, so a change to the
        /// tag schema reds here instead of following the producer silently.
        fn att_reference() -> String {
            format!("8.8.8.8/acme/tool:sha256-{}.att", subject_digest().hex())
        }

        /// The `.att` sidecar manifest the run published, parsed.
        fn stored_sidecar(transport: &RecordingTransport) -> ImageManifest {
            let bytes = transport
                .stored(&att_reference())
                .unwrap_or_else(|| panic!("no sidecar manifest at {}", att_reference()));
            serde_json::from_slice(&bytes).expect("the sidecar is an image manifest")
        }

        /// The default, and the shape every invocation that predates the flag gets:
        /// an OCI referrer and no sidecar tag at all.
        #[tokio::test]
        async fn bundle_writes_the_referrer_and_no_att_sidecar() {
            let run = drive_format(SignatureFormat::Bundle).await;
            let result = run.result.expect("run succeeded");

            assert!(result.referrer.is_some(), "the bundle shape publishes a referrer");
            assert!(result.sidecar.is_none(), "no sidecar was asked for");
            assert!(
                run.transport.stored(&att_reference()).is_none(),
                "an .att tag was written for --signature-format bundle"
            );
            assert!(
                run.transport
                    .calls()
                    .iter()
                    .any(|call| call.starts_with("push_referrer_manifest:")),
                "the referrer manifest was never pushed: {:?}",
                run.transport.calls()
            );
        }

        /// The pin the spec's §Registry-visible names states: the sidecar tag is
        /// written **only** under `simplesigning`/`both`, and under `simplesigning`
        /// the referrer is not written at all.
        #[tokio::test]
        async fn simplesigning_writes_the_att_sidecar_and_no_referrer() {
            let run = drive_format(SignatureFormat::Simplesigning).await;
            let result = run.result.expect("run succeeded");

            assert!(result.sidecar.is_some(), "the sidecar shape publishes a sidecar");
            assert!(
                result.referrer.is_none(),
                "--signature-format simplesigning must not publish a referrer bundle"
            );
            assert_eq!(stored_sidecar(&run.transport).layers.len(), 1);
            assert!(
                !run.transport
                    .calls()
                    .iter()
                    .any(|call| call.starts_with("push_referrer_manifest:")),
                "a referrer manifest was pushed for --signature-format simplesigning: {:?}",
                run.transport.calls()
            );
        }

        /// `both` is the union, and the two publications carry the SAME signature —
        /// one `sign_dsse` call, not two. Signing twice would spend two Fulcio
        /// certificates on identical content and let the two publications disagree
        /// about which identity attested.
        #[tokio::test]
        async fn both_writes_each_shape_from_one_signature() {
            let run = drive_format(SignatureFormat::Both).await;
            let result = run.result.expect("run succeeded");

            assert!(result.referrer.is_some(), "the referrer half of `both` is missing");
            assert!(result.sidecar.is_some(), "the sidecar half of `both` is missing");
            assert!(run.transport.stored(&att_reference()).is_some());
            assert!(
                run.transport
                    .calls()
                    .iter()
                    .any(|call| call.starts_with("push_referrer_manifest:")),
            );
            // `signed_bytes` panics unless there was exactly one call.
            run.signer.signed_bytes();
        }

        /// The wire shape of the layer itself: a bare DSSE envelope, typed
        /// `application/vnd.dsse.envelope.v1+json`, addressed by its own SHA-256.
        ///
        /// Not the Sigstore bundle: cosign's `.att` tag predates bundles and has
        /// always held the envelope, so publishing the bundle here would be a tag
        /// no cosign reader can use.
        #[tokio::test]
        async fn the_att_layer_is_the_dsse_envelope_itself() {
            let run = drive_format(SignatureFormat::Simplesigning).await;
            let expected = test_envelope(&run.signer.signed_bytes());
            let manifest = stored_sidecar(&run.transport);
            let [layer] = manifest.layers.as_slice() else {
                panic!("expected exactly one layer, got {}", manifest.layers.len());
            };

            assert_eq!(
                layer.media_type, DSSE_ENVELOPE_MEDIA_TYPE,
                "the .att layer must be typed as a DSSE envelope"
            );
            assert_eq!(
                layer.digest,
                Algorithm::Sha256.hash(&expected).to_string(),
                "the layer must address the envelope the signer returned, not the bundle wrapping it"
            );
            assert_eq!(layer.size, expected.len() as i64);
        }

        /// The verification material a cosign reader needs — including the
        /// detached-signature key, written empty.
        ///
        /// **Inverted 2026-08-30.** This test used to assert the opposite: that
        /// `dev.cosignproject.cosign/signature` must be *absent*, on the
        /// reasoning that a DSSE envelope carries its signatures inside (in
        /// `signatures[].sig`) so an empty value would claim material that is
        /// not there. The reasoning is sound about the *value* and wrong about
        /// the *key*. On an `.att` layer cosign reads the key as a presence
        /// marker: `cosign verify-attestation` refuses a layer without it
        /// ("signature layer sha256:… is missing dev.cosignproject.cosign/signature
        /// annotation"), so the old contract published `.att` sidecars no cosign
        /// release can verify — which no OCX-side test could see. cosign's own
        /// `attach attestation` writes the key with an empty value, pinned by
        /// `test/tests/fixtures/golden/attestation_sidecar_key_manifest.json`.
        ///
        /// The empty value is therefore asserted exactly, not merely tolerated:
        /// anything else in that position would be material claimed but not
        /// carried, which is what the original reasoning correctly forbids.
        #[tokio::test]
        async fn the_att_layer_carries_the_cosign_verification_annotations_and_an_empty_signature_marker() {
            let run = drive_format(SignatureFormat::Simplesigning).await;
            let manifest = stored_sidecar(&run.transport);
            let annotations = manifest.layers[0]
                .annotations
                .as_ref()
                .expect("the layer carries verification material");

            assert_eq!(
                annotations.get(ANNOTATION_COSIGN_CERTIFICATE).map(String::as_str),
                Some(TEST_CERTIFICATE_PEM),
            );
            assert_eq!(
                annotations.get(ANNOTATION_COSIGN_BUNDLE).map(String::as_str),
                Some(TEST_REKOR_BUNDLE),
            );
            assert_eq!(
                annotations.get(ANNOTATION_COSIGN_SIGNATURE).map(String::as_str),
                Some(""),
                "cosign refuses an .att layer with no signature annotation, and writes the key \
             empty itself; the value must stay empty because the envelope carries the signature \
             inside: {annotations:?}"
            );
        }

        /// A second attestation over the same subject **appends**. Replacing would
        /// silently delete an attestation someone else published under the same
        /// mutable tag.
        #[tokio::test]
        async fn a_second_attestation_appends_a_layer_rather_than_replacing_one() {
            let transport = RecordingTransport::default();
            let first = drive_attest_with(
                transport.clone(),
                AttestMode::Signed,
                PredicateType::CycloneDx,
                PREDICATE,
                false,
                false,
                SignatureFormat::Simplesigning,
            )
            .await;
            first.result.expect("the first attestation lands");
            let first_layer = stored_sidecar(&transport).layers[0].digest.clone();

            // A different predicate, therefore a different Statement, therefore a
            // different envelope — the shape two real signings always have.
            let second = drive_attest_with(
                transport.clone(),
                AttestMode::Signed,
                PredicateType::CycloneDx,
                r#"{"bomFormat":"CycloneDX","specVersion":"1.5"}"#,
                false,
                false,
                SignatureFormat::Simplesigning,
            )
            .await;
            second.result.expect("the second attestation lands");

            let layers = stored_sidecar(&transport).layers;
            assert_eq!(layers.len(), 2, "the second attestation replaced the first");
            assert_eq!(
                layers[0].digest, first_layer,
                "the first attestation must survive the append, in place"
            );
            assert_ne!(layers[1].digest, first_layer);
        }

        /// An unsigned attach has no DSSE envelope, so it cannot write an `.att`
        /// layer — and quietly writing the referrer instead would make the flag
        /// mean something it does not say. Refused before any network contact.
        #[tokio::test]
        async fn an_unsigned_attach_refuses_a_sidecar_request() {
            for format in [SignatureFormat::Simplesigning, SignatureFormat::Both] {
                let run = drive_attest_with(
                    RecordingTransport::default(),
                    AttestMode::Unsigned,
                    PredicateType::CycloneDx,
                    PREDICATE,
                    false,
                    false,
                    format,
                )
                .await;

                let Err(error) = run.result else {
                    panic!("an unsigned attach must refuse --signature-format {format}");
                };
                assert!(
                    matches!(error.kind, SignErrorKind::SidecarRequiresSignature { .. }),
                    "expected the sidecar refusal for {format}, got: {error}"
                );
                assert_eq!(classify_error(&error), ExitCode::UsageError);
                assert!(
                    run.transport.calls().is_empty(),
                    "the refusal must land before any registry contact: {:?}",
                    run.transport.calls()
                );
            }
        }
    }
}
