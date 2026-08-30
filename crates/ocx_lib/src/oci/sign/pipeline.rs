// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sign pipeline — the push-side state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md):
//! resolve the per-platform target manifest → check Referrers-API capability →
//! acquire an OIDC token → produce a Sigstore bundle (delegated to a
//! [`Signer`]) → push the bundle blob → push the referrer manifest whose
//! `subject` points at the target.
//!
//! The pipeline is a thin orchestrator: the cryptographic signing is delegated
//! to a [`Signer`] trait object and the registry writes go through the injected
//! [`OciTransport`].
//!
//! Two things ADR S1-F forbade now happen here, both by later decision. A
//! registry with no Referrers API gets the OCI tag-schema fallback index rather
//! than a refusal (Amendment 10), and `--signature-format simplesigning|both`
//! writes the cosign `sha256-<hex>.sig` sidecar — a second, independent
//! signature over a different payload, not a re-packaging of the bundle.

use url::Url;

use super::error::{SignError, SignErrorKind};
use super::oidc::TokenProvider;
use super::signer::Signer;
use crate::file_structure::StateStore;
use crate::oci::attest::COSIGN_SIGN_PREDICATE_TYPE;
use crate::oci::attest::statement;
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport};
use crate::oci::index::{Index, IndexOperation};
use crate::oci::referrer::ReferrerManifest;
use crate::oci::referrer::manifest::{bundle_annotations, bundle_created, bundle_now};
use crate::oci::referrer::media_types::{
    BUNDLE_CONTENT_DSSE, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_PAYLOAD, SIGSTORE_BUNDLE_V03,
};
use crate::oci::resolve_target::{ResolveTargetError, SignTarget, resolve_sign_target};
use crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE;
use crate::oci::sign::format::SignatureFormat;
use crate::oci::sign::referrers::{attach_referrer, map_client_error, referrers_capability};
use crate::oci::sign::simplesigning_write;
use crate::oci::simplesigning::SimpleSigningClaim;
use crate::oci::{Descriptor, Digest, Identifier, OCI_IMAGE_MEDIA_TYPE, Platform, native};

/// Manifest media types accepted when fetching the per-platform target.
const ACCEPTED_MANIFEST_TYPES: &[&str] = &[
    OCI_IMAGE_MEDIA_TYPE,
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// Context passed into [`SignPipeline::run`] — all external dependencies.
pub struct SignContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Narrowing selector, when one was requested.
    ///
    /// `None` acts on whatever the reference resolved to — an index is then
    /// the subject itself. `Some` narrows into an index and acts on that
    /// child, and is an error when the resolution was not an index. The rule
    /// itself lives in [`resolve_sign_target`], shared with attest and verify.
    pub platform: Option<&'a Platform>,
    /// Signer producing the cryptographic bundle.
    pub signer: &'a dyn Signer,
    /// OIDC token provider (override → ambient → browser dispatch).
    pub token_provider: &'a dyn TokenProvider,
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// Index for resolving tag → per-platform manifest digest.
    pub index: &'a Index,
    /// Fulcio URL (validated at the CLI boundary).
    pub fulcio_url: &'a Url,
    /// Rekor URL (validated at the CLI boundary).
    pub rekor_url: &'a Url,
    /// State store owning the referrers-capability cache layout.
    pub state: &'a StateStore,
    /// Which wire shape(s) to write (spec D8). `Bundle` is the default; each
    /// selected format is an **independent** signature with its own Fulcio
    /// certificate and its own Rekor entry.
    pub format: SignatureFormat,
}

/// Result emitted by a sign pipeline run.
///
/// "Successful" is per **leg**: `--signature-format both` writes two
/// independent signatures, and the run is best-effort per leg rather than
/// atomic (spec D8). A leg that failed is reported alongside one that
/// succeeded; the caller decides the exit code from [`Self::first_failure`].
pub struct SignResult {
    /// Digest of the target manifest the signature(s) were attached to.
    pub subject_digest: Digest,
    /// Per-format outcome, in write order. Never empty.
    pub legs: Vec<SignatureLeg>,
    /// Cert SAN (identity) that signed the target — the OIDC subject.
    ///
    /// From the first leg that succeeded. Every leg redeems the *same* OIDC
    /// token, so their certificates carry the same SAN by construction; the
    /// field would only differ if two legs somehow used two identities, which
    /// no code path allows.
    pub certificate_identity: String,
    /// Cert issuer (`--certificate-oidc-issuer` comparand) — the OIDC issuer.
    pub certificate_oidc_issuer: String,
    /// Which key model produced the signature(s).
    pub key_backend: crate::oci::sign::KeyBackendKind,
    /// The signing key's cosign hint, in key mode only.
    pub public_key_hint: Option<String>,
    /// The Rekor log index of the first successful leg, when one was created.
    ///
    /// Reported rather than inferred: under a key `--rekor-upload` is opt-in,
    /// so its absence is a legal outcome the operator must be able to see.
    pub transparency_log_index: Option<u64>,
}

impl SignResult {
    /// The first leg that failed, if any — the run's exit-code source.
    ///
    /// `None` means every selected format was written.
    #[must_use]
    pub fn first_failure(&self) -> Option<&SignErrorKind> {
        self.legs.iter().find_map(|leg| leg.outcome.as_ref().err())
    }
}

/// One wire shape's outcome.
pub struct SignatureLeg {
    /// The shape this leg wrote.
    pub format: SignatureFormat,
    /// What it produced, or why it did not.
    pub outcome: Result<LegDigests, SignErrorKind>,
}

/// The two addresses a written signature occupies.
#[derive(Debug)]
pub struct LegDigests {
    /// The signed payload's blob digest: the Sigstore bundle under `bundle`,
    /// the simplesigning claim under `simplesigning` — and, on the attest side,
    /// the bare DSSE envelope the `.att` sidecar carries.
    pub payload_digest: Digest,
    /// The manifest the payload hangs from: the OCI referrer under `bundle`,
    /// the `sha256-<hex>.sig` (or, attesting, `.att`) sidecar under
    /// `simplesigning`.
    pub manifest_digest: Digest,
}

/// The identity facts a report reads, whichever leg produced them.
///
/// Cloned into [`SignResult`] from the first leg that succeeded. Every leg
/// redeems the same OIDC token, so under keyless they agree by construction.
#[derive(Clone)]
struct SignedIdentity {
    certificate_identity: String,
    certificate_oidc_issuer: String,
    key_backend: crate::oci::sign::KeyBackendKind,
    public_key_hint: Option<String>,
    transparency_log_index: Option<u64>,
}

impl Default for SignedIdentity {
    /// Hand-written because `KeyBackendKind` is a frozen G1 type with no
    /// `Default`, and giving one a default key model there would let an
    /// unpopulated report claim `keyless`. Here the value is only ever reached
    /// when no leg succeeded, which the caller has already turned into an
    /// error.
    fn default() -> Self {
        Self {
            certificate_identity: String::new(),
            certificate_oidc_issuer: String::new(),
            key_backend: crate::oci::sign::KeyBackendKind::Keyless,
            public_key_hint: None,
            transparency_log_index: None,
        }
    }
}

/// The SAN of a PEM leaf certificate, using the verify side's own extractor so
/// the two commands cannot drift into disagreeing about one certificate.
fn identity_from_pem(pem: &str) -> Option<String> {
    parse_leaf(pem).and_then(|leaf| crate::oci::verify::identity::subject_identity(&leaf))
}

/// The Fulcio issuer extension of a PEM leaf certificate.
fn issuer_from_pem(pem: &str) -> Option<String> {
    parse_leaf(pem).and_then(|leaf| crate::oci::verify::identity::oidc_issuer(&leaf))
}

/// Decode a PEM leaf into the X.509 type the extractors take.
fn parse_leaf(pem: &str) -> Option<x509_cert::Certificate> {
    use x509_cert::der::DecodePem as _;
    x509_cert::Certificate::from_pem(pem.as_bytes()).ok()
}

/// Apply the SSRF floor/// Apply the SSRF floor to exactly the Sigstore endpoints `signer` will dial.
///
/// Shared with the attest pipeline: both reach the same two services under the
/// same rule, and a second copy would be a second place for "which endpoint is
/// live in which key model" to drift.
pub(crate) async fn guard_dialed_endpoints(
    trusted: &[String],
    signer: &dyn Signer,
    fulcio_url: &Url,
    rekor_url: &Url,
) -> Result<(), SignErrorKind> {
    let endpoints = [
        (signer.requires_identity_token(), fulcio_url, "--fulcio-url"),
        (signer.uploads_to_transparency_log(), rekor_url, "--rekor-url"),
    ];
    for (dialed, url, flag) in endpoints {
        if !dialed {
            continue;
        }
        crate::oci::endpoint::resolve_sigstore_url(url, trusted)
            .await
            .map_err(|error| SignErrorKind::InvalidEndpointUrl {
                endpoint: flag.into(),
                reason: crate::oci::endpoint::UrlRejection::from(error),
            })?;
    }
    Ok(())
}

/// Resolve what a sign or attest run acts on, applying the `--platform`
/// optionality rule.
///
/// Shared with the attest pipeline, which faces the identical question against
/// the identical taxonomy; verify wires the same decision into its own error
/// kinds. The rule itself is [`resolve_sign_target`]'s and lives nowhere else,
/// so the three verbs cannot answer a reference differently.
///
/// Resolution goes **through the index chain**, not the registry transport: the
/// transport path bypasses `guard_local_physical` and the mirror map, and would
/// break `--offline`. One fetch answers both questions the rule asks — which
/// digest the reference names, and whether that object is an index listing
/// children.
///
/// # Errors
///
/// [`SignErrorKind::TargetNotFound`] when the reference resolves to nothing, or
/// to an index with no single compatible child;
/// [`SignErrorKind::TargetNotAnIndex`] when a platform was requested and the
/// resolution is a bare manifest;
/// [`SignErrorKind::SubjectDigestUnsupported`] when the resolved subject is not
/// addressed by sha256.
pub(crate) async fn resolve_platform_target(
    index: &Index,
    identifier: &Identifier,
    platform: Option<&Platform>,
) -> Result<SignTarget, SignErrorKind> {
    let Some((resolved_digest, manifest)) = index
        .fetch_manifest(identifier, IndexOperation::Resolve)
        .await
        .map_err(|e| SignErrorKind::Internal(Box::new(e)))?
    else {
        return Err(SignErrorKind::TargetNotFound {
            platform: platform_label(platform),
        });
    };
    // `None` for a bare image manifest — resolution reached the acted-on object
    // directly and there is no index to narrow into. That distinction, not the
    // reference's form, is what the rule branches on.
    let children: Option<Vec<(Platform, Digest)>> = match &manifest {
        crate::oci::Manifest::ImageIndex(index) => Some(
            index
                .manifests
                .iter()
                .filter_map(|entry| {
                    // The one shared eligibility rule, same as
                    // `Index::fetch_candidates`: an entry naming no platform (or
                    // one OCX cannot represent) is a referrer entry, not
                    // something `--platform` can ever mean.
                    let platform = Platform::candidate_from_descriptor(entry)?;
                    Some((platform, Digest::try_from(entry.digest.clone()).ok()?))
                })
                .collect(),
        ),
        crate::oci::Manifest::Image(_) => None,
    };
    let target =
        resolve_sign_target(&resolved_digest, children.as_deref(), platform).map_err(map_resolve_target_error)?;
    // The sha256 floor, applied at the one seam both `sign` and `attest` pass
    // through and before either has written anything. Not in
    // `resolve_sign_target`: that module is the `--platform` rule and verify
    // shares it, and verify must keep *reading* whatever a registry holds.
    //
    // Both artifacts OCX writes are sha256-only, and both fail later and worse
    // than a refusal here — see `SubjectDigestUnsupported`. `--platform` is
    // what makes this reachable without a hand-written digest: the child
    // descriptor's algorithm is the index author's choice, and nothing between
    // here and the write narrows it.
    let algorithm = target.subject_digest.algorithm();
    if algorithm != crate::oci::Algorithm::Sha256 {
        return Err(SignErrorKind::SubjectDigestUnsupported {
            algorithm: algorithm.prefix().to_owned(),
        });
    }
    Ok(target)
}

/// How a `--platform` request reads in a message when none was made.
///
/// The flag is optional, so "no manifest for platform " with nothing after it
/// is reachable; `any` is what the absence means — act on whatever resolved.
/// Same spelling verify uses for the same absence.
fn platform_label(platform: Option<&Platform>) -> String {
    platform.map_or_else(|| "any".to_string(), Platform::to_string)
}

/// Map the shared `--platform` decision's refusals into the sign taxonomy.
///
/// `PlatformNotFound` and `AmbiguousPlatform` land on
/// [`SignErrorKind::TargetNotFound`], which is where the required-`--platform`
/// `Index::select` refusals landed too — message and exit code unchanged for
/// them.
fn map_resolve_target_error(error: ResolveTargetError) -> SignErrorKind {
    match error {
        ResolveTargetError::NotAnIndex { platform } => SignErrorKind::TargetNotAnIndex { platform },
        ResolveTargetError::PlatformNotFound { platform } | ResolveTargetError::AmbiguousPlatform { platform } => {
            SignErrorKind::TargetNotFound { platform }
        }
    }
}

/// Sign pipeline entry point.
pub struct SignPipeline;

impl SignPipeline {
    /// Run the push-side sign state machine.
    ///
    /// The registry transport is derived from `client` internally, so the
    /// public API never exposes `&dyn OciTransport` (ADR Amendment 1, Option 3).
    pub async fn run(client: &Client, ctx: SignContext<'_>) -> Result<SignResult, SignError> {
        let identifier = ctx.identifier.clone();
        Self::run_inner(client, ctx)
            .await
            .map_err(|kind| SignError::new(identifier, kind))
    }

    async fn run_inner(client: &Client, ctx: SignContext<'_>) -> Result<SignResult, SignErrorKind> {
        // 0. SSRF floor for the trust services (CWE-918). The CLI boundary
        //    validated these URLs as *strings*; this is where we find out where
        //    they actually resolve, before anything dials them.
        //
        //    Guarded per endpoint the run will actually dial, which the signer
        //    answers for. Guarding an endpoint that is never contacted is not
        //    caution but a false dependency: a key-mode sign in an air-gapped
        //    org reaches no Fulcio and, by default, no Rekor, and resolving
        //    either would fail the signature on DNS for a host it never opens a
        //    socket to.
        guard_dialed_endpoints(
            ctx.index.trusted_hosts_for(ctx.identifier.registry()),
            ctx.signer,
            ctx.fulcio_url,
            ctx.rekor_url,
        )
        .await?;

        let transport = client.transport();
        // 1. Resolve the target manifest under the `--platform` optionality
        //    rule. `enclosing_index` is verify's to read (the membership test);
        //    signing acts on the subject and nothing else.
        let SignTarget { subject_digest, .. } =
            resolve_platform_target(ctx.index, ctx.identifier, ctx.platform).await?;
        let resolved = ctx.identifier.clone_with_digest(subject_digest.clone());
        // Index indirection: a logical name (`ocx.sh/<ns>/<pkg>`) may point at a
        // different physical registry, so every transport-facing call below —
        // subject fetch, capability probe, blob + referrer push — targets the
        // physical address. `Ok(None)` = no rewrite, same contract the pull
        // path's `resolve_transport_pinned` reads. The SSRF floor on the
        // returned host is enforced upstream in the shared index choke point
        // (`ChainedIndex::guard_local_physical`), never re-checked here.
        let physical = ctx
            .index
            .physical_reference(&resolved)
            .await
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?
            .unwrap_or_else(|| resolved.clone());
        // The pre-flight above had to tolerate a DNS lookup failure -- it runs on
        // every resolve, including ones that never fetch, so it cannot fail on a
        // missing resolver. Its own contract says the tolerance is safe only
        // because the dial site re-validates fail-closed, which is here: a
        // request is now imminent, and the shared client carries no SSRF
        // resolver of its own. Without this the sign/verify paths held only the
        // tolerant half of that split (CWE-918). Same call the pull path makes.
        ctx.index
            .guard_physical_dial(&resolved, &physical)
            .await
            .map_err(|error| SignErrorKind::ForbiddenRegistryTarget {
                reason: error.to_string(),
            })?;
        // Two seams, because signing both reads and writes. The subject fetch is
        // a read and may be served by a `[mirrors]` entry; the referrer push is
        // a write and must reach the canonical host — remote/proxy mirrors are
        // read-only (ADR Q5), so a signature pushed at a mirror is rejected, or
        // against a writable mirror lands where the canonical verifier never
        // looks. Same Pull/Push split `Client::ensure_auth` makes.
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
        // `subject_bytes.len()` becomes the subject descriptor's `size` pushed
        // to the CANONICAL host, but the bytes came from the READ host, which
        // under a `[mirrors]` entry is a different machine. Bind them to the
        // digest already resolved before trusting their length — a wrong size
        // yields a signature strict verifiers reject. Fail closed, reusing the
        // transport's own wrong-content error so the exit code matches a
        // registry-served mismatch anywhere else.
        if Digest::try_from(served_digest.as_str()).ok().as_ref() != Some(&subject_digest) {
            return Err(map_client_error(ClientError::DigestMismatch {
                expected: subject_digest.to_string(),
                actual: served_digest,
            }));
        }

        // 2. Referrers-API capability (cache-first) of the host we will PUSH
        //    to. A mirror's referrers support says nothing about the upstream's,
        //    and the upstream is where the referrer manifest has to land.
        //
        // The verdict is still read, because it decides whether the tag-schema
        // fallback index is written alongside the manifest; what changed is that
        // it no longer decides whether signing may happen at all.
        let referrers_support =
            referrers_capability(transport, &write_image, &subject_digest, ctx.state, ctx.no_cache).await?;

        // 3. Acquire the OIDC token — keyless only. A key-mode signature has no
        //    identity to prove, so asking for one would fail a signature that
        //    needs none.
        let token = match ctx.signer.requires_identity_token() {
            true => Some(ctx.token_provider.acquire("sigstore").await?),
            false => None,
        };

        // 4. Write each selected shape. `--signature-format both` emits two
        //    INDEPENDENT signatures, each with its own Fulcio certificate and
        //    its own Rekor entry — a simplesigning signature covers a different
        //    payload, so it cannot be re-packaged from the bundle.
        //
        //    Best-effort per leg, never atomic (spec D8): one shape landing and
        //    the other failing is a real outcome, and hiding the successful one
        //    behind the failure would leave the operator re-signing what is
        //    already published. The caller reads `first_failure` for the exit
        //    code.
        let subject_descriptor = Descriptor {
            media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: subject_digest.to_string(),
            size: subject_bytes.len() as i64,
            ..Descriptor::default()
        };
        let mut legs = Vec::new();
        let mut identity = None;

        if ctx.format.writes_bundle() {
            let outcome = Self::write_bundle_leg(
                transport,
                &ctx,
                &write_image,
                &physical,
                &subject_digest,
                subject_descriptor.clone(),
                token.as_ref(),
                referrers_support,
            )
            .await;
            if let Ok((digests, signed)) = &outcome {
                let _ = digests;
                identity.get_or_insert_with(|| signed.clone());
            }
            legs.push(SignatureLeg {
                format: SignatureFormat::Bundle,
                outcome: outcome.map(|(digests, _)| digests),
            });
        }

        if ctx.format.writes_simplesigning() {
            let outcome = Self::write_simplesigning_leg(
                transport,
                &ctx,
                &write_image,
                &physical,
                &subject_digest,
                token.as_ref(),
            )
            .await;
            if let Ok((_, signed)) = &outcome {
                identity.get_or_insert_with(|| signed.clone());
            }
            legs.push(SignatureLeg {
                format: SignatureFormat::Simplesigning,
                outcome: outcome.map(|(digests, _)| digests),
            });
        }

        // Every selected leg failed: there is nothing to report, so the run
        // fails outright with the first cause rather than emitting a success
        // envelope listing only failures.
        if legs.iter().all(|leg| leg.outcome.is_err()) {
            let first = legs
                .into_iter()
                .find_map(|leg| leg.outcome.err())
                .unwrap_or_else(|| SignErrorKind::Internal("no signature format was selected".into()));
            return Err(first);
        }

        let identity = identity.unwrap_or_default();
        Ok(SignResult {
            subject_digest,
            legs,
            certificate_identity: identity.certificate_identity,
            certificate_oidc_issuer: identity.certificate_oidc_issuer,
            key_backend: identity.key_backend,
            public_key_hint: identity.public_key_hint,
            transparency_log_index: identity.transparency_log_index,
        })
    }

    /// The `bundle` leg: a cosign-shaped DSSE image signature published as an
    /// OCI referrer.
    ///
    /// Returns the leg's digests alongside the identity facts the report reads,
    /// so a caller that runs two legs takes them from whichever succeeded first.
    #[allow(clippy::too_many_arguments)]
    async fn write_bundle_leg(
        transport: &dyn OciTransport,
        ctx: &SignContext<'_>,
        write_image: &native::Reference,
        physical: &Identifier,
        subject_digest: &Digest,
        subject_descriptor: Descriptor,
        token: Option<&crate::oci::sign::OidcToken>,
        referrers_support: crate::oci::referrer::capability::ReferrersSupport,
    ) -> Result<(LegDigests, SignedIdentity), SignErrorKind> {
        // cosign v3 signs an image by wrapping the digest in a DSSE in-toto
        // Statement whose `predicateType` is
        // `https://sigstore.dev/cosign/sign/v1` and whose predicate is empty —
        // NOT by putting the digest in a `messageSignature`. The subject digest
        // is what binds; the name is informational. Same statement shape as an
        // attestation, so the same `sign_dsse` machinery produces it.
        let statement = statement::build_image_signature(physical.repository(), subject_digest);
        let statement_bytes = serde_json::to_vec(&statement).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let bundle = ctx
            .signer
            .sign_dsse(&statement_bytes, token, ctx.fulcio_url, ctx.rekor_url)
            .await?;

        // Push the referrer's blobs: the OCI empty-config blob (the manifest's
        // `config` descriptor points at it) and the Sigstore bundle blob (the
        // `layers[0]` payload). A spec-strict registry (zot) rejects the
        // manifest with MANIFEST_INVALID if either referenced blob is absent,
        // so both must land before the manifest PUT. `push_blob` HEADs first,
        // so re-pushing the shared empty-config blob is a no-op after the first.
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
        transport
            .push_blob(write_image, bundle.bytes.clone(), &bundle.digest, no_progress)
            .await
            .map_err(map_client_error)?;

        let bundle_descriptor = Descriptor {
            media_type: BUNDLE_V03_MEDIA_TYPE.to_string(),
            digest: bundle.digest.to_string(),
            size: bundle.bytes.len() as i64,
            ..Descriptor::default()
        };
        // cosign parity: the referrer annotations name the bundle's content
        // oneof and, since the signature is now a DSSE Statement, its
        // predicateType — the pair that lets a listing tell an image signature
        // from an attestation without fetching the blob. Byte-for-byte what
        // cosign v3.1.1 wrote in
        // `test/tests/fixtures/golden/keyless_referrer_manifest.json`.
        let annotations = bundle_annotations(
            &bundle_created(bundle_now()),
            BUNDLE_CONTENT_DSSE,
            COSIGN_SIGN_PREDICATE_TYPE,
        );
        let manifest = ReferrerManifest::build(
            subject_descriptor,
            SIGSTORE_BUNDLE_V03,
            bundle_descriptor,
            Some(annotations),
        );
        let manifest_bytes = manifest.to_canonical_json()?;
        // `write_image` is `transport_write_reference`'s, never the mirrored read
        // reference: the fallback append PUTs a tag through whatever host it is
        // handed, and a signature written to a mirror is one the canonical
        // verifier never looks at (CWE-345/367, `oci/client.rs:164-181`).
        let (manifest_digest, _descriptor) = attach_referrer(
            transport,
            write_image,
            subject_digest,
            &manifest_bytes,
            referrers_support,
        )
        .await?;

        Ok((
            LegDigests {
                payload_digest: bundle.digest,
                manifest_digest,
            },
            SignedIdentity {
                certificate_identity: bundle.certificate_identity,
                certificate_oidc_issuer: bundle.certificate_oidc_issuer,
                key_backend: bundle.key_backend,
                public_key_hint: bundle.public_key_hint,
                transparency_log_index: bundle.transparency_log_index,
            },
        ))
    }

    /// The `simplesigning` leg: a cosign `sha256-<hex>.sig` sidecar.
    ///
    /// The claim bytes are what gets signed, so this is a second signature over
    /// a different payload rather than a repackaging of the bundle.
    async fn write_simplesigning_leg(
        transport: &dyn OciTransport,
        ctx: &SignContext<'_>,
        write_image: &native::Reference,
        physical: &Identifier,
        subject_digest: &Digest,
        token: Option<&crate::oci::sign::OidcToken>,
    ) -> Result<(LegDigests, SignedIdentity), SignErrorKind> {
        // The reference cosign records in the claim: registry + repository, no
        // tag and no digest. The digest is carried separately, in the field a
        // verifier actually binds on.
        let docker_reference = format!("{}/{}", physical.registry(), physical.repository());
        let claim = SimpleSigningClaim::new(docker_reference, subject_digest);
        // Signed as served, never re-serialized: `to_signing_bytes` is the one
        // producer of these bytes, and the layer's SHA-256 is their address.
        let payload = claim
            .to_signing_bytes()
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        let signed = ctx
            .signer
            .sign_blob(&payload, token, ctx.fulcio_url, ctx.rekor_url)
            .await?;
        let payload_digest = crate::oci::Algorithm::Sha256.hash(&payload);
        let layer = simplesigning_write::SidecarLayer::signature(payload, &signed);
        let manifest_digest = simplesigning_write::append_layer(transport, write_image, subject_digest, &layer).await?;

        Ok((
            LegDigests {
                payload_digest,
                manifest_digest,
            },
            SignedIdentity {
                // A simplesigning signature carries its certificate in an
                // annotation rather than a bundle, so the identity is read back
                // out of the same PEM the layer holds.
                certificate_identity: signed
                    .certificate_pem
                    .as_deref()
                    .and_then(identity_from_pem)
                    .unwrap_or_default(),
                certificate_oidc_issuer: signed
                    .certificate_pem
                    .as_deref()
                    .and_then(issuer_from_pem)
                    .unwrap_or_default(),
                key_backend: signed.key_backend,
                public_key_hint: signed.public_key_hint,
                transparency_log_index: signed.transparency_log_index,
            },
        ))
    }

    // The Unsupported verdict no longer refuses the operation: the OCI referrers
    // tag-schema fallback (`list_referrers_with_fallback` /
    // `append_referrer_fallback_index`) serves a registry without the Referrers
    // API. See `adr_oci_referrers_signing_v1.md`, Amendment 10 — the fallback
    // index is a mutable tag anyone with push access authors, and the residual
    // attack surface that reverses S1-F is recorded there.
    //
    // `ensure_referrers_supported` stood here and raised
    // `SignErrorKind::ReferrersUnsupported` (exit 84) on that verdict. Its
    // cache-first probe survives as `sign::referrers::referrers_capability`,
    // which returns the verdict rather than refusing on it.
}

#[cfg(test)]
mod platform_narrowing_tests {
    //! WP1: `--platform` is a narrowing modifier, not a required selector.
    //!
    //! `resolve_target.rs` owns the rule and tests it against a candidate list
    //! handed to it directly. What is untested there — and is the whole of what
    //! this pipeline seam adds — is the step before: turning a resolved
    //! `Manifest` into that candidate list, and turning the rule's refusals into
    //! this side's error kinds. A bug in either is invisible to `resolve_target`'s
    //! own suite, so each row below drives the seam end to end.

    use super::*;
    use crate::cli::ExitCode;
    use crate::cli::classify::ClassifyErrorKind as _;
    use crate::oci::index::IndexImpl;
    use crate::oci::{INDEX_SCHEMA_VERSION, ImageIndex, ImageIndexEntry, ImageManifest};

    fn digest(byte: u8) -> Digest {
        Digest::Sha256(format!("{byte:064x}"))
    }

    fn platform(value: &str) -> Platform {
        value.parse::<Platform>().expect("test platform parses")
    }

    /// A child descriptor naming `os/arch`, the shape a real image index holds.
    fn child(os: native::Os, architecture: native::Arch, byte: u8) -> ImageIndexEntry {
        ImageIndexEntry {
            digest: digest(byte).to_string(),
            media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
            size: 1,
            platform: Some(native::Platform {
                os,
                architecture,
                variant: None,
                features: None,
                os_version: None,
                os_features: None,
            }),
            artifact_type: None,
            annotations: None,
        }
    }

    /// An index fake that answers every resolve with one fixed manifest, so a
    /// test picks the RESOLUTION SHAPE rather than the reference form — which
    /// is exactly the axis the rule branches on.
    #[derive(Clone)]
    struct ShapedIndex {
        resolved: Digest,
        manifest: crate::oci::Manifest,
    }

    #[async_trait::async_trait]
    impl IndexImpl for ShapedIndex {
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
            Ok(Some((self.resolved.clone(), self.manifest.clone())))
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(self.resolved.clone()))
        }

        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// An index fake whose resolve finds nothing at all.
    #[derive(Clone)]
    struct EmptyIndex;

    #[async_trait::async_trait]
    impl IndexImpl for EmptyIndex {
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
            Ok(None)
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(None)
        }

        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn identifier() -> Identifier {
        Identifier::parse("registry.example/acme/tool:1.0").expect("identifier")
    }

    fn bare_manifest_index() -> Index {
        Index::from_impl(ShapedIndex {
            resolved: digest(0xaa),
            manifest: crate::oci::Manifest::Image(ImageManifest::default()),
        })
    }

    fn image_index() -> Index {
        Index::from_impl(ShapedIndex {
            resolved: digest(0xaa),
            manifest: crate::oci::Manifest::ImageIndex(ImageIndex {
                schema_version: INDEX_SCHEMA_VERSION,
                media_type: Some(crate::oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
                artifact_type: None,
                manifests: vec![
                    child(native::Os::Linux, native::Arch::Amd64, 0x01),
                    child(native::Os::Linux, native::Arch::ARM64, 0x02),
                ],
                annotations: None,
            }),
        })
    }

    /// A sha384 digest. 96 hex characters, and a perfectly legal OCI address —
    /// which is the point: nothing stops an index author from publishing one.
    fn sha384(byte: u8) -> Digest {
        Digest::Sha384(format!("{byte:096x}"))
    }

    /// An index whose one `linux/arm64` child is addressed by `child_digest`.
    fn index_listing(child_digest: &Digest) -> Index {
        let mut entry = child(native::Os::Linux, native::Arch::ARM64, 0x02);
        entry.digest = child_digest.to_string();
        Index::from_impl(ShapedIndex {
            resolved: digest(0xaa),
            manifest: crate::oci::Manifest::ImageIndex(ImageIndex {
                schema_version: INDEX_SCHEMA_VERSION,
                media_type: Some(crate::oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
                artifact_type: None,
                manifests: vec![entry],
                annotations: None,
            }),
        })
    }

    /// **A subject OCX cannot address in a cosign artifact is refused here**,
    /// at resolution, before a blob, a sidecar tag or a Rekor entry exists.
    ///
    /// `--platform` is what makes this reachable with nothing hand-written: the
    /// child descriptor's algorithm is the index author's choice, and nothing
    /// between here and the write narrowed it. The digest then travelled
    /// verbatim into both artifacts, each of which fails later and worse — the
    /// sidecar tag was spelled with 96 hex characters and read back with 64
    /// (exit 79 `no_signatures_found`, for a signature OCX had just written),
    /// and the in-toto Statement carried a DigestSet `binds_subject` refuses,
    /// discovered only after a permanent transparency-log entry was burned.
    ///
    /// The sha256 control comes first, through the same helper: without it a
    /// resolution broken in some unrelated way would refuse vacuously.
    #[tokio::test]
    async fn a_non_sha256_subject_is_refused_before_anything_is_written() {
        // The control narrows through `--platform` too, so it exercises the
        // same child-selection path the refusal takes; a control that only
        // signed the index would leave that path unproven in the green half.
        let accepted = resolve_platform_target(
            &index_listing(&digest(0x02)),
            &identifier(),
            Some(&platform("linux/arm64")),
        )
        .await
        .expect("a sha256 child is signable");
        assert_eq!(accepted.subject_digest, digest(0x02));
        assert_eq!(accepted.subject_digest.algorithm(), crate::oci::Algorithm::Sha256);

        let narrowed = resolve_platform_target(
            &index_listing(&sha384(0x02)),
            &identifier(),
            Some(&platform("linux/arm64")),
        )
        .await
        .expect_err("cosign addresses its subject by sha256 alone");
        assert!(
            matches!(&narrowed, SignErrorKind::SubjectDigestUnsupported { algorithm } if algorithm == "sha384"),
            "expected SubjectDigestUnsupported naming sha384, got {narrowed:?}",
        );
        assert_eq!(narrowed.kind_detail(), "subject_digest_unsupported");
        assert_eq!(narrowed.exit_code(), ExitCode::DataError);

        // The flagless path too: `--platform` is only the cheapest way in, not
        // the only one. A reference resolving straight onto a sha384 manifest
        // is the same refusal.
        let bare = Index::from_impl(ShapedIndex {
            resolved: sha384(0xaa),
            manifest: crate::oci::Manifest::Image(ImageManifest::default()),
        });
        let resolved = resolve_platform_target(&bare, &identifier(), None)
            .await
            .expect_err("the resolved object is the subject, and it is sha384");
        assert_eq!(resolved.kind_detail(), "subject_digest_unsupported");
    }

    /// The absent flag on a BARE MANIFEST: sign what resolved.
    #[tokio::test]
    async fn no_platform_against_a_bare_manifest_acts_on_the_resolved_object() {
        let target = resolve_platform_target(&bare_manifest_index(), &identifier(), None)
            .await
            .expect("a bare manifest needs no narrowing");
        assert_eq!(target.subject_digest, digest(0xaa));
        assert_eq!(target.enclosing_index, None);
    }

    /// The absent flag on an INDEX: sign the index itself, not a child. This is
    /// the row `--platform required` made unreachable, and the one cosign's
    /// multi-platform tag signature lives on.
    #[tokio::test]
    async fn no_platform_against_an_index_signs_the_index_itself() {
        let target = resolve_platform_target(&image_index(), &identifier(), None)
            .await
            .expect("an index is a legal subject");
        assert_eq!(
            target.subject_digest,
            digest(0xaa),
            "the subject must be the index digest, never a child's"
        );
        assert_eq!(target.enclosing_index, None);
    }

    /// The present flag on an INDEX: narrow to that child.
    #[tokio::test]
    async fn a_platform_against_an_index_narrows_to_that_child() {
        let target = resolve_platform_target(&image_index(), &identifier(), Some(&platform("linux/arm64")))
            .await
            .expect("arm64 is listed");
        assert_eq!(target.subject_digest, digest(0x02));
        assert_eq!(target.enclosing_index, Some(digest(0xaa)));
    }

    /// The present flag on a BARE MANIFEST: refused, with a kind of its own.
    ///
    /// The slug is byte-identical to verify's for the same refusal, and the
    /// exit code is 79 — the same code `--platform typo` already produced, so a
    /// `case $?` contract is untouched while the word tells the two apart.
    #[tokio::test]
    async fn a_platform_against_a_bare_manifest_is_refused_as_not_an_index() {
        let error = resolve_platform_target(&bare_manifest_index(), &identifier(), Some(&platform("linux/amd64")))
            .await
            .expect_err("there is nothing to narrow into");
        assert!(
            matches!(&error, SignErrorKind::TargetNotAnIndex { platform } if platform == "linux/amd64"),
            "expected TargetNotAnIndex, got {error:?}"
        );
        assert_eq!(error.kind_detail(), "target_not_an_index");
        assert_eq!(error.exit_code(), ExitCode::NotFound);
        assert_eq!(
            error.kind_detail(),
            crate::oci::verify::VerifyErrorKind::TargetNotAnIndex {
                platform: "linux/amd64".into(),
            }
            .kind_detail(),
            "one refusal, one word, whichever verb reported it",
        );
    }

    /// A platform the index does not list keeps the pre-existing answer:
    /// `target_not_found`, exit 79. Nothing about widening the flag may
    /// reclassify a plain typo.
    #[tokio::test]
    async fn a_platform_the_index_does_not_list_is_still_target_not_found() {
        let error = resolve_platform_target(&image_index(), &identifier(), Some(&platform("windows/amd64")))
            .await
            .expect_err("windows is not listed");
        assert_eq!(error.kind_detail(), "target_not_found");
        assert_eq!(error.exit_code(), ExitCode::NotFound);
    }

    /// A reference that resolves to nothing reports the platform it was asked
    /// for, and `any` when it was asked for none — the label must not render as
    /// an empty tail.
    #[tokio::test]
    async fn an_unresolvable_reference_labels_the_absent_platform_as_any() {
        let index = Index::from_impl(EmptyIndex);
        let error = resolve_platform_target(&index, &identifier(), None)
            .await
            .expect_err("nothing resolved");
        assert!(
            matches!(&error, SignErrorKind::TargetNotFound { platform } if platform == "any"),
            "expected the `any` label, got {error:?}"
        );
        let named = resolve_platform_target(&index, &identifier(), Some(&platform("linux/amd64")))
            .await
            .expect_err("nothing resolved");
        assert!(
            matches!(&named, SignErrorKind::TargetNotFound { platform } if platform == "linux/amd64"),
            "expected the requested platform, got {named:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ExitCode, classify_error};
    use crate::oci::client::OciTransport;
    use crate::oci::native;
    use crate::oci::referrer::capability::ReferrersApiCapability;

    #[test]
    fn map_client_error_preserves_referrers_unsupported() {
        let mapped = map_client_error(ClientError::ReferrersUnsupported {
            registry: "example.com".to_string(),
        });
        assert!(matches!(mapped, SignErrorKind::ReferrersUnsupported));
    }

    #[test]
    fn map_client_error_wraps_other_errors_as_internal() {
        let mapped = map_client_error(ClientError::InvalidManifest("bad".to_string()));
        assert!(matches!(mapped, SignErrorKind::Internal(_)));
    }

    #[test]
    fn map_client_error_keeps_invalid_image_index_internal_and_still_exits_65() {
        // The sign taxonomy has no exit-65 kind whose message fits a registry-side
        // malformed image index (`RekorSetMalformed` would blame the transparency
        // log), so the kind stays `Internal` -- but the exit code must not: the
        // wrapped `ClientError` classifies itself as `DataError`, and
        // `SignError::classify` deferring on `Internal` is what lets it through.
        let index: crate::oci::ImageIndex =
            serde_json::from_slice(br#"{"schemaVersion":1,"manifests":[]}"#).expect("image index json");
        let invalid = crate::oci::manifest::validate_image_index(&index).expect_err("schemaVersion 1 is refused");
        let kind = map_client_error(ClientError::InvalidImageIndex(invalid));
        assert!(matches!(kind, SignErrorKind::Internal(_)));
        assert_eq!(classify_error(&SignError::new(sign_id(), kind)), ExitCode::DataError);
    }

    /// A registry fault during signing must reach the operator as the registry's
    /// own exit code, not the catch-all 1.
    ///
    /// Failure this pins: `map_client_error` sinks every non-referrers
    /// `ClientError` into `SignErrorKind::Internal`, and `SignError::classify`
    /// used to answer `Some(Failure)` unconditionally -- so the outer wrapper
    /// short-circuited its own cause and a CI pipeline written to the documented
    /// contract (`case $? in 75) retry;; 80) refresh-creds;;`) never fired.
    /// Both halves are asserted together on purpose: a test that constructed
    /// `Internal` by hand would still pass if `map_client_error` stopped
    /// producing it.
    #[test]
    fn registry_faults_keep_their_own_exit_codes_through_sign() {
        let cases = [
            (
                ClientError::RegistryTransient(Box::new(std::io::Error::other("503 from registry"))),
                ExitCode::TempFail,
            ),
            (
                ClientError::Authentication(Box::new(std::io::Error::other("401 from registry"))),
                ExitCode::AuthError,
            ),
            (
                ClientError::Registry(Box::new(std::io::Error::other("registry said no"))),
                ExitCode::Unavailable,
            ),
        ];
        for (client_error, expected) in cases {
            let rendered = client_error.to_string();
            let err = SignError::new(sign_id(), map_client_error(client_error));
            assert_eq!(classify_error(&err), expected, "client error: {rendered}");
        }
    }

    /// The other half of the deferral contract: an `Internal` whose cause no
    /// classifier recognizes must still exit 1, via `classify_error`'s
    /// fall-through rather than an assertion at the wrapper.
    #[test]
    fn unclassifiable_internal_still_exits_failure_through_sign() {
        let kind = SignErrorKind::Internal("something no classifier knows".into());
        assert_eq!(classify_error(&SignError::new(sign_id(), kind)), ExitCode::Failure);
    }

    fn sign_id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    // ── Index indirection: transport traffic follows the PHYSICAL registry ──

    /// SHA-256 the indirecting test index reports as the subject digest.
    fn indirection_subject_digest() -> Digest {
        crate::oci::Algorithm::Sha256.hash(b"indirected subject manifest")
    }

    /// A test index whose logical name resolves to a DIFFERENT physical
    /// registry — the `index.ocx.sh` shape (`ocx.sh/<ns>/<pkg>` pointing at
    /// `oci://8.8.8.8/<org>/<repo>`) reduced to what the pipeline consumes.
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
                indirection_subject_digest(),
                crate::oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )))
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(indirection_subject_digest()))
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

    /// Transport double that records `"<method>:<registry>"` for every call, so
    /// a test can assert which host the pipeline actually talked to. Only the
    /// methods this pipeline reaches do any work.
    #[derive(Clone, Default)]
    struct RecordingTransport {
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// When set, `pull_manifest_raw` claims this digest instead of the
        /// resolved subject digest — a mirror serving the wrong manifest.
        served_subject_digest: Option<String>,
        /// When true, `list_referrers` answers `ReferrersUnsupported` — the
        /// `registry:2` shape, where the tag-schema fallback is the only way a
        /// signature becomes discoverable.
        referrers_unsupported: bool,
        /// Tag-addressed manifest store, so the fallback index's
        /// read-append-write-read-back loop runs for real rather than against a
        /// double that cannot hold what it just wrote.
        manifests: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>>,
        /// Every referrer manifest pushed, verbatim — the bytes a cosign reader
        /// would see, so a test can assert on annotations rather than on the
        /// exit code.
        referrer_manifests: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        /// When true, the FIRST manifest PUT to a `.sig` tag is answered `Ok`
        /// and then overwritten by a rival's document — the lost update a plain
        /// read-append-write cannot see, and the reason the append reads back.
        clobber_first_sidecar_put: bool,
        /// Whether the clobber has already fired.
        clobbered: std::sync::Arc<std::sync::Mutex<bool>>,
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

        /// The bytes stored at `reference`, if any — how a test reads back the
        /// fallback index this transport was asked to hold.
        fn stored(&self, reference: &str) -> Option<Vec<u8>> {
            self.manifests
                .lock()
                .expect("manifest store lock")
                .get(reference)
                .cloned()
        }

        /// Place `bytes` at `reference` before the run, standing in for a
        /// document some other client published.
        fn seed(&self, reference: &str, bytes: Vec<u8>) {
            self.manifests
                .lock()
                .expect("manifest store lock")
                .insert(reference.to_string(), bytes);
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
            unimplemented!("sign never lists tags")
        }

        async fn catalog(
            &self,
            _: &native::Reference,
            _: usize,
            _: Option<String>,
        ) -> std::result::Result<Vec<String>, ClientError> {
            unimplemented!("sign never reads the catalog")
        }

        async fn fetch_manifest_digest(&self, _: &native::Reference) -> std::result::Result<String, ClientError> {
            unimplemented!("sign resolves digests through the index")
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
                let digest = crate::oci::Algorithm::Sha256.hash(&bytes).to_string();
                return Ok((bytes, digest));
            }
            if image.tag().is_some_and(|tag| tag.starts_with("sha256-")) {
                return Err(ClientError::ManifestNotFound(image.whole()));
            }
            let digest = self
                .served_subject_digest
                .clone()
                .unwrap_or_else(|| indirection_subject_digest().to_string());
            Ok((b"{}".to_vec(), digest))
        }

        async fn pull_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<Vec<u8>, ClientError> {
            unimplemented!("sign never pulls blobs")
        }

        async fn pull_blob_to_file(
            &self,
            _: &native::Reference,
            _: &Digest,
            _: &std::path::Path,
        ) -> std::result::Result<(), ClientError> {
            unimplemented!("sign never pulls blobs")
        }

        async fn head_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<u64, ClientError> {
            unimplemented!("sign never HEADs blobs")
        }

        async fn push_manifest(
            &self,
            _: &native::Reference,
            _: &crate::oci::Manifest,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("sign pushes the referrer manifest through push_referrer_manifest")
        }

        async fn push_manifest_raw(
            &self,
            image: &native::Reference,
            bytes: Vec<u8>,
            _: &str,
        ) -> std::result::Result<String, ClientError> {
            // Records the whole reference, not just the registry: the fallback
            // index is identified by its TAG, and a recorder that drops the tag
            // cannot tell a fallback write from any other manifest PUT — the
            // exact blind spot Amendment 10 found in the old ADR test tape.
            self.calls
                .lock()
                .expect("recorder lock")
                .push(format!("push_manifest_raw:{}", image.whole()));
            let key = image.whole();
            let rival = self.clobber_first_sidecar_put
                && key.ends_with(".sig")
                && !std::mem::replace(&mut *self.clobbered.lock().expect("clobber lock"), true);
            let stored = if rival {
                // A concurrent writer read the same base manifest and pushed
                // after us: our PUT returned `Ok` and its layer is gone.
                serde_json::to_vec(&serde_json::json!({
                    "schemaVersion": 2,
                    "mediaType": OCI_IMAGE_MEDIA_TYPE,
                    "config": {
                        "mediaType": "application/vnd.oci.empty.v1+json",
                        "digest": crate::oci::referrer::media_types::EMPTY_CONFIG_DIGEST,
                        "size": 2,
                    },
                    "layers": [{
                        "mediaType": crate::oci::referrer::media_types::SIMPLESIGNING_MEDIA_TYPE,
                        "digest": format!("sha256:{}", "cc".repeat(32)),
                        "size": 9,
                        "annotations": { "dev.cosignproject.cosign/signature": "cml2YWw=" },
                    }],
                }))
                .expect("rival manifest")
            } else {
                bytes
            };
            self.manifests
                .lock()
                .expect("manifest store lock")
                .insert(key.clone(), stored);
            Ok(key)
        }

        async fn push_blob(
            &self,
            image: &native::Reference,
            _: Vec<u8>,
            digest: &Digest,
            _: std::sync::Arc<dyn Fn(u64) + Send + Sync>,
        ) -> std::result::Result<String, ClientError> {
            self.record("push_blob", image);
            Ok(digest.to_string())
        }

        async fn push_blob_from_path(
            &self,
            image: &native::Reference,
            path: &std::path::Path,
            digest: &Digest,
            on_progress: std::sync::Arc<dyn Fn(u64) + Send + Sync>,
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
            // just pushed, exactly as `NativeTransport` does. A double that left
            // them `None` would pass a test asserting they survive the fallback
            // append while the real transport's value was never exercised.
            let manifest: ReferrerManifest =
                serde_json::from_slice(manifest_bytes).expect("the pipeline pushes a referrer manifest");
            Ok(Descriptor {
                media_type: media_type.to_string(),
                digest: crate::oci::Algorithm::Sha256.hash(manifest_bytes).to_string(),
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
            // A successful (empty) listing is what the capability probe reads as
            // "this registry supports the Referrers API"; the refusal is what it
            // reads as `Unsupported`.
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

    struct FixedTokenProvider;

    #[async_trait::async_trait]
    impl TokenProvider for FixedTokenProvider {
        async fn acquire(&self, _: &str) -> Result<super::super::oidc::OidcToken, SignErrorKind> {
            Ok(super::super::oidc::OidcToken::new(jwt_with_payload(
                &serde_json::json!({ "sub": "me@example.com", "iss": "https://issuer.example" }),
            )))
        }
    }

    /// Records the statement bytes it was asked to sign, so a test can assert
    /// the DSSE payload's shape rather than merely that signing happened.
    #[derive(Clone, Default)]
    struct FixedSigner {
        statement: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
        blob_payload: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[async_trait::async_trait]
    impl Signer for FixedSigner {
        async fn sign_blob(
            &self,
            payload: &[u8],
            _: Option<&super::super::oidc::OidcToken>,
            _: &Url,
            _: &Url,
        ) -> Result<crate::oci::sign::SignedBlob, SignErrorKind> {
            // Records the claim bytes, so a test can assert what was signed
            // rather than merely that signing happened.
            *self.blob_payload.lock().expect("payload lock") = Some(payload.to_vec());
            Ok(crate::oci::sign::SignedBlob {
                signature: b"test-signature".to_vec(),
                certificate_pem: Some("-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----\n".to_string()),
                chain_pem: None,
                rekor_bundle: Some(r#"{"SignedEntryTimestamp":"c2V0"}"#.to_string()),
                transparency_log_index: Some(7),
                key_backend: crate::oci::sign::KeyBackendKind::Keyless,
                public_key_hint: None,
            })
        }

        async fn sign_dsse(
            &self,
            statement_bytes: &[u8],
            _: Option<&super::super::oidc::OidcToken>,
            _: &Url,
            _: &Url,
        ) -> Result<crate::oci::sign::bundle::SignedBundle, SignErrorKind> {
            // Records what it was handed, so a test can assert the pipeline
            // signed an in-toto Statement rather than a bare digest.
            *self.statement.lock().expect("statement lock") = Some(statement_bytes.to_vec());
            let bytes = br#"{"mediaType":"test-bundle"}"#.to_vec();
            let digest = crate::oci::Algorithm::Sha256.hash(&bytes);
            Ok(crate::oci::sign::bundle::SignedBundle {
                key_backend: crate::oci::sign::KeyBackendKind::Keyless,
                public_key_hint: None,
                transparency_log_index: Some(1),
                bytes,
                digest,
                certificate_identity: "me@example.com".to_string(),
                certificate_oidc_issuer: "https://issuer.example".to_string(),
                envelope_json: br#"{"payloadType":"application/vnd.in-toto+json"}"#.to_vec(),
                certificate_pem: None,
                rekor_bundle: None,
            })
        }

        fn signer_kind(&self) -> &'static str {
            "test-fixed"
        }
    }

    /// Drive a full sign run against the recording transport for the logical
    /// name `ocx.sh/acme/tool:1.0`, indirected to the physical
    /// `8.8.8.8/acme/tool:1.0`. Returns the `"<method>:<registry>"` log plus the
    /// state dir, so a caller can read back the persisted capability record.
    /// Panics if the run does not complete.
    async fn run_recorded_sign(mirrors: crate::oci::client::MirrorMap) -> (Vec<String>, tempfile::TempDir) {
        let (result, calls, temp, _signer) = drive_sign(mirrors, RecordingTransport::default()).await;
        if let Err(error) = result {
            panic!("sign must complete against the recording transport: {error}");
        }
        (calls, temp)
    }

    /// The run itself, with the transport injectable and the outcome returned
    /// rather than asserted — so a test can drive a failing sign.
    async fn drive_sign(
        mirrors: crate::oci::client::MirrorMap,
        transport: RecordingTransport,
    ) -> (
        Result<SignResult, SignError>,
        Vec<String>,
        tempfile::TempDir,
        FixedSigner,
    ) {
        // A public IP literal, not a name: the pipeline now resolves the physical
        // host before dialing it (dial-site SSRF guard), and an IP literal resolves
        // locally -- a DNS name here would make this unit test open a socket.
        drive_sign_at("8.8.8.8/acme/tool:1.0", mirrors, transport, SignatureFormat::Bundle).await
    }

    /// `drive_sign` with the physical registry the index rewrites to made an
    /// argument, so a test can point the indirection at a forbidden target.
    async fn drive_sign_at(
        physical: &str,
        mirrors: crate::oci::client::MirrorMap,
        transport: RecordingTransport,
        format: SignatureFormat,
    ) -> (
        Result<SignResult, SignError>,
        Vec<String>,
        tempfile::TempDir,
        FixedSigner,
    ) {
        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let physical = Identifier::parse(physical).expect("physical identifier");

        let mut client = Client::with_transport(Box::new(transport.clone()));
        client.mirrors = mirrors;
        let index = Index::from_impl(IndirectingIndex { physical });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        // Loopback rather than a `.example` name: the pipeline's dial-time SSRF
        // guard resolves the endpoint before use, and a documentation domain does
        // not resolve -- which would make this unit test depend on DNS. Loopback is
        // also what a real local stack looks like.
        let fulcio_url = Url::parse("http://127.0.0.1:5555").expect("fulcio url");
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        // `None`, not `Some(Platform::any())`: `IndirectingIndex` resolves to a
        // bare image manifest, and under the narrowing rule a platform against
        // a bare manifest is `TargetNotAnIndex`. Signing whatever resolved is
        // what this fixture is about.
        let signer = FixedSigner::default();
        let token_provider = FixedTokenProvider;

        let result = SignPipeline::run(
            &client,
            SignContext {
                format,
                identifier: &logical,
                platform: None,
                signer: &signer,
                token_provider: &token_provider,
                no_cache: true,
                index: &index,
                fulcio_url: &fulcio_url,
                rekor_url: &rekor_url,
                state: &state,
            },
        )
        .await;
        (result, transport.calls(), temp, signer)
    }

    // ── WP5b: the cosign simplesigning sidecar ──

    /// The `.sig` sidecar tag for `indirection_subject_digest()`.
    fn sidecar_reference() -> String {
        format!("8.8.8.8/acme/tool:sha256-{}.sig", indirection_subject_digest().hex())
    }

    /// Drive a run at `format` and hand back the transport so a test can read
    /// what landed.
    async fn drive_format(format: SignatureFormat) -> (SignResult, RecordingTransport, FixedSigner) {
        let transport = RecordingTransport::default();
        let (result, _calls, _temp, signer) = drive_sign_at(
            "8.8.8.8/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
            transport.clone(),
            format,
        )
        .await;
        (result.expect("signing must complete"), transport, signer)
    }

    /// The sidecar manifest stored at the `.sig` tag, parsed.
    fn stored_sidecar(transport: &RecordingTransport) -> serde_json::Value {
        let bytes = transport
            .stored(&sidecar_reference())
            .expect("the simplesigning sidecar was written");
        serde_json::from_slice(&bytes).expect("the sidecar is JSON")
    }

    /// WP5b round trip: layer media type, the claim bytes as the layer, and the
    /// cosign annotations a reader looks for.
    ///
    /// The claim is signed as **opaque bytes**, so the layer's digest is the
    /// digest of exactly what was signed — asserted here rather than trusted.
    #[tokio::test]
    async fn a_simplesigning_sidecar_carries_the_claim_and_its_cosign_annotations() {
        let (result, transport, signer) = drive_format(SignatureFormat::Simplesigning).await;

        let payload = signer
            .blob_payload
            .lock()
            .expect("payload lock")
            .clone()
            .expect("the pipeline signed a claim");
        let claim: serde_json::Value = serde_json::from_slice(&payload).expect("the claim is JSON");
        assert_eq!(
            claim["critical"]["image"]["docker-manifest-digest"],
            indirection_subject_digest().to_string(),
            "the claim binds the subject digest a verifier checks",
        );
        assert_eq!(
            claim["critical"]["type"],
            crate::oci::simplesigning::SIMPLESIGNING_CLAIM_TYPE
        );
        assert_eq!(claim["critical"]["identity"]["docker-reference"], "8.8.8.8/acme/tool");

        let sidecar = stored_sidecar(&transport);
        let layers = sidecar["layers"].as_array().expect("the sidecar has layers");
        assert_eq!(layers.len(), 1, "one signature, one layer");
        assert_eq!(
            layers[0]["mediaType"],
            crate::oci::referrer::media_types::SIMPLESIGNING_MEDIA_TYPE,
        );
        assert_eq!(
            layers[0]["digest"],
            crate::oci::Algorithm::Sha256.hash(&payload).to_string(),
            "the layer addresses exactly the bytes that were signed",
        );

        let annotations = &layers[0]["annotations"];
        assert!(
            annotations["dev.cosignproject.cosign/signature"].is_string(),
            "the signature annotation is what cosign reads first: {annotations}",
        );
        assert!(
            annotations["dev.sigstore.cosign/certificate"].is_string(),
            "a keyless sidecar carries its Fulcio leaf in an annotation: {annotations}",
        );
        assert!(
            annotations["dev.sigstore.cosign/bundle"].is_string(),
            "the offline Rekor bundle rides the same annotation set: {annotations}",
        );
        // `/chain` is absent, and legally so: bundle v0.3 dropped the chain
        // field, Fulcio's intermediates come from the trust root, and cosign
        // v3.1.1's own `.sig` manifests carry no `/chain` either.
        assert!(annotations["dev.sigstore.cosign/chain"].is_null());

        let leg = result.legs.first().expect("one leg");
        assert_eq!(leg.format, SignatureFormat::Simplesigning);
        assert!(leg.outcome.is_ok(), "the simplesigning leg must have landed");
    }

    /// The claim bytes this harness's runs sign, and their digest.
    ///
    /// A simplesigning claim carries no `optional` section, so it is a pure
    /// function of the repository and the subject digest — which is exactly why
    /// a second signer's layer has the **same address** as ours and can only be
    /// told apart by its signature annotation.
    fn harness_claim_digest() -> Digest {
        let claim = SimpleSigningClaim::new("8.8.8.8/acme/tool", &indirection_subject_digest());
        crate::oci::Algorithm::Sha256.hash(claim.to_signing_bytes().expect("claim serializes"))
    }

    /// Re-signing **appends** rather than replaces: a second signature over the
    /// same subject — a second signer, a re-sign after a key rotation — must not
    /// silently delete the first.
    ///
    /// The seeded layer carries **our own payload digest** under someone else's
    /// signature, because that is the only shape a second signer of this subject
    /// can have. A foreign digest would make this pass against a writer that
    /// deduped on the address alone, which is the defect the layer identity
    /// predicate exists to stop.
    #[tokio::test]
    async fn re_signing_appends_a_layer_rather_than_replacing_the_sidecar() {
        let transport = RecordingTransport::default();
        let their_digest = harness_claim_digest().to_string();
        let foreign = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_IMAGE_MEDIA_TYPE,
            "config": {
                "mediaType": "application/vnd.oci.empty.v1+json",
                "digest": crate::oci::referrer::media_types::EMPTY_CONFIG_DIGEST,
                "size": 2,
            },
            "layers": [{
                "mediaType": crate::oci::referrer::media_types::SIMPLESIGNING_MEDIA_TYPE,
                "digest": their_digest,
                "size": 12,
                "annotations": { "dev.cosignproject.cosign/signature": "c29tZW9uZS1lbHNl" },
            }],
        });
        transport.seed(
            &sidecar_reference(),
            serde_json::to_vec(&foreign).expect("seed manifest"),
        );

        let (result, _calls, _temp, _signer) = drive_sign_at(
            "8.8.8.8/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
            transport.clone(),
            SignatureFormat::Simplesigning,
        )
        .await;
        result.expect("signing must complete against an existing sidecar");

        let sidecar = stored_sidecar(&transport);
        let layers = sidecar["layers"].as_array().expect("layers");
        assert_eq!(layers.len(), 2, "the existing signature survives: {sidecar}");
        assert_eq!(
            layers[0]["annotations"]["dev.cosignproject.cosign/signature"], "c29tZW9uZS1lbHNl",
            "the pre-existing signature stays first and unmodified: {sidecar}",
        );
        assert_ne!(
            layers[1]["annotations"]["dev.cosignproject.cosign/signature"],
            layers[0]["annotations"]["dev.cosignproject.cosign/signature"],
            "the appended layer must be OUR signature, not a copy of theirs: {sidecar}",
        );
    }

    /// The other half of the same identity question: a sidecar that already
    /// holds **our** signature is not appended to twice.
    ///
    /// Without it the predicate could be "annotations differ" alone, and a retry
    /// after an ambiguous read-back would stack a duplicate every time.
    #[tokio::test]
    async fn a_sidecar_already_holding_this_signature_is_left_alone() {
        let transport = RecordingTransport::default();
        let (first, calls, _temp, _signer) = drive_sign_at(
            "8.8.8.8/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
            transport.clone(),
            SignatureFormat::Simplesigning,
        )
        .await;
        first.expect("the first signature lands");
        let published = stored_sidecar(&transport);
        assert_eq!(published["layers"].as_array().expect("layers").len(), 1);
        let _ = calls;

        // Re-run `append_signature` with the very layer that landed, which is
        // what a retry does after a read-back it could not confirm.
        // The same signature bytes `FixedSigner` produced above: a retry
        // re-presents the layer it already built, annotation included.
        let signed = crate::oci::sign::SignedBlob {
            signature: b"test-signature".to_vec(),
            certificate_pem: None,
            chain_pem: None,
            rekor_bundle: None,
            transparency_log_index: None,
            key_backend: crate::oci::sign::KeyBackendKind::Keyless,
            public_key_hint: None,
        };
        let claim = SimpleSigningClaim::new("8.8.8.8/acme/tool", &indirection_subject_digest());
        let payload = claim.to_signing_bytes().expect("claim serializes");
        let image = native::Reference::try_from("8.8.8.8/acme/tool:1.0").expect("reference");
        let before = transport.stored(&sidecar_reference()).expect("sidecar exists");
        let layer = simplesigning_write::SidecarLayer::signature(payload, &signed);
        simplesigning_write::append_layer(&transport, &image, &indirection_subject_digest(), &layer)
            .await
            .expect("a re-append of the same signature is a no-op, not a failure");
        let after = transport.stored(&sidecar_reference()).expect("sidecar exists");
        assert_eq!(before, after, "the sidecar must not have been rewritten");
    }

    /// The concurrent case D4 names: a writer that clobbers our PUT is caught
    /// by the read-back, and the retry lands our layer on top of theirs.
    ///
    /// Modelled by a transport that drops the FIRST manifest PUT to the sidecar
    /// tag and substitutes a rival's — precisely the lost update a plain
    /// read-append-write cannot see.
    #[tokio::test]
    async fn a_clobbered_sidecar_write_is_caught_by_the_read_back_and_retried() {
        let transport = RecordingTransport {
            clobber_first_sidecar_put: true,
            ..RecordingTransport::default()
        };
        let (result, calls, _temp, _signer) = drive_sign_at(
            "8.8.8.8/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
            transport.clone(),
            SignatureFormat::Simplesigning,
        )
        .await;
        result.expect("the append must converge rather than lose the signature");

        let sidecar_puts = calls
            .iter()
            .filter(|call| call == &&format!("push_manifest_raw:{}", sidecar_reference()))
            .count();
        assert!(
            sidecar_puts >= 2,
            "the read-back must have forced a retry; puts were {calls:?}",
        );

        let sidecar = stored_sidecar(&transport);
        let layers = sidecar["layers"].as_array().expect("layers");
        assert!(
            layers
                .iter()
                .any(|layer| layer["annotations"]["dev.cosignproject.cosign/signature"]
                    == serde_json::json!("dGVzdC1zaWduYXR1cmU=")),
            "our signature must be present after the retry: {sidecar}",
        );
        assert!(
            layers
                .iter()
                .any(|layer| layer["digest"] == serde_json::json!(format!("sha256:{}", "cc".repeat(32)))),
            "the rival's layer must survive too — an append never deletes: {sidecar}",
        );
    }

    /// `--signature-format both` emits **each** shape: an OCI referrer and a
    /// `.sig` sidecar, from two independent signatures.
    #[tokio::test]
    async fn signature_format_both_emits_each_shape() {
        let (result, transport, _signer) = drive_format(SignatureFormat::Both).await;

        let formats: Vec<SignatureFormat> = result.legs.iter().map(|leg| leg.format).collect();
        assert_eq!(formats, vec![SignatureFormat::Bundle, SignatureFormat::Simplesigning]);
        assert!(result.legs.iter().all(|leg| leg.outcome.is_ok()), "both legs must land",);

        assert_eq!(
            transport.referrer_manifests.lock().expect("recorder lock").len(),
            1,
            "the bundle leg publishes exactly one OCI referrer",
        );
        assert!(
            transport.stored(&sidecar_reference()).is_some(),
            "the simplesigning leg publishes the sidecar tag",
        );
    }

    /// The default is `bundle` alone: no sidecar tag is written unless asked
    /// for. A second wire shape nobody selected is a second thing to verify and
    /// a second thing to get wrong.
    #[tokio::test]
    async fn the_default_format_writes_no_simplesigning_sidecar() {
        let (result, transport, _signer) = drive_format(SignatureFormat::Bundle).await;
        assert_eq!(result.legs.len(), 1);
        assert_eq!(result.legs[0].format, SignatureFormat::Bundle);
        assert!(
            transport.stored(&sidecar_reference()).is_none(),
            "no sidecar without --signature-format simplesigning|both",
        );
    }

    // ── WP3: the image signature is a cosign-shaped DSSE Statement ──

    /// The statement bytes the pipeline handed the signer, as JSON.
    async fn signed_statement() -> serde_json::Value {
        let (result, _calls, _temp, signer) =
            drive_sign(crate::oci::client::MirrorMap::default(), RecordingTransport::default()).await;
        result.expect("sign must complete against the recording transport");
        let bytes = signer
            .statement
            .lock()
            .expect("statement lock")
            .clone()
            .expect("the pipeline signed a DSSE statement");
        serde_json::from_slice(&bytes).expect("the signed payload is an in-toto Statement")
    }

    /// WP3 / spec D2. cosign v3 signs an image by wrapping the digest in a DSSE
    /// in-toto Statement with an **empty** predicate under the cosign
    /// image-signature predicateType — not by putting the digest in a
    /// `messageSignature`.
    ///
    /// Asserted on the payload shape rather than on the exit code: a run that
    /// signed the wrong bytes exits 0 just as happily as one that signed the
    /// right ones.
    #[tokio::test]
    async fn the_image_signature_payload_is_a_cosign_shaped_in_toto_statement() {
        let statement = signed_statement().await;

        assert_eq!(statement["_type"], "https://in-toto.io/Statement/v1");
        assert_eq!(statement["predicateType"], COSIGN_SIGN_PREDICATE_TYPE);
        assert_eq!(
            statement["predicate"],
            serde_json::json!({}),
            "the cosign image-signature predicate is empty",
        );

        let subjects = statement["subject"].as_array().expect("subject array");
        assert_eq!(subjects.len(), 1, "one subject: the digest being signed");
        assert_eq!(
            subjects[0]["digest"]["sha256"],
            indirection_subject_digest().hex(),
            "the subject digest is the manifest the signature covers",
        );
    }

    /// The referrer's annotations are the listing-time discriminator: a cosign
    /// reader tells an image signature from an attestation by
    /// `content` + `predicateType` without fetching the blob. Both must move
    /// with the payload, or a DSSE signature is advertised as a
    /// `message-signature` and skipped.
    #[tokio::test]
    async fn the_signature_referrer_announces_a_dsse_envelope_and_the_cosign_predicate_type() {
        let transport = RecordingTransport::default();
        let (result, _calls, _temp, _signer) =
            drive_sign(crate::oci::client::MirrorMap::default(), transport.clone()).await;
        result.expect("sign must complete");

        let pushed = transport.referrer_manifests.lock().expect("recorder lock").clone();
        let [bytes] = pushed.as_slice() else {
            panic!("expected exactly one referrer manifest push, got {}", pushed.len());
        };
        let manifest: serde_json::Value = serde_json::from_slice(bytes).expect("referrer manifest is JSON");

        assert_eq!(
            manifest["annotations"]["dev.sigstore.bundle.content"],
            BUNDLE_CONTENT_DSSE
        );
        assert_eq!(
            manifest["annotations"]["dev.sigstore.bundle.predicateType"],
            COSIGN_SIGN_PREDICATE_TYPE,
        );
        assert_eq!(manifest["artifactType"], SIGSTORE_BUNDLE_V03);
    }

    /// The referrers fallback tag for `indirection_subject_digest()`.
    fn fallback_tag_reference() -> String {
        format!("8.8.8.8/acme/tool:sha256-{}", indirection_subject_digest().hex())
    }

    /// **C-009, in the positive.** A registry with no Referrers API used to
    /// refuse the whole sign with exit 84; it now gets the OCI tag-schema
    /// fallback index written alongside the referrer manifest.
    ///
    /// Loop A shipped `append_referrer_fallback_index` with no production
    /// caller, and `-D dead-code` cannot fire on a `pub` trait method in a
    /// library crate — so nothing in the build would have noticed if this
    /// wiring never happened. This is the check that notices.
    #[tokio::test]
    async fn a_registry_without_the_referrers_api_gets_the_fallback_index_written() {
        let transport = RecordingTransport {
            referrers_unsupported: true,
            ..RecordingTransport::default()
        };
        let (result, calls, _temp, _signer) =
            drive_sign(crate::oci::client::MirrorMap::default(), transport.clone()).await;
        result.expect("signing must succeed on a registry without the Referrers API");

        let tag_reference = fallback_tag_reference();
        assert!(
            calls.contains(&format!("push_manifest_raw:{tag_reference}")),
            "the fallback index tag must be written, got: {calls:?}",
        );

        // Spec step 5 is a MUST, and the half sigstore/cosign#4641 gets wrong:
        // the appended descriptor keeps the referrer's `artifactType`.
        let stored = transport.stored(&tag_reference).expect("the fallback index was stored");
        let index: crate::oci::ImageIndex = serde_json::from_slice(&stored).expect("fallback index parses");
        assert_eq!(index.manifests.len(), 1, "one referrer descriptor appended");
        assert_eq!(
            index.manifests[0].artifact_type.as_deref(),
            Some(SIGSTORE_BUNDLE_V03),
            "artifactType survives the append",
        );
    }

    /// The fallback write goes to the CANONICAL host, never the mirror.
    ///
    /// `sibling_tag_reference` propagates whatever host it is handed, so a
    /// mirrored read reference here would PUT signatures to a read-only mirror —
    /// deciding on one host while writing to another (CWE-345/367).
    #[tokio::test]
    async fn the_fallback_index_is_written_to_the_canonical_host_not_the_mirror() {
        let mirrors = crate::oci::client::MirrorMap::new([(
            "8.8.8.8".to_string(),
            crate::config::mirror::ParsedMirror {
                protocol: "https".to_string(),
                host: "mirror.example".to_string(),
                path_prefix: "proxy".to_string(),
            },
        )]);
        let transport = RecordingTransport {
            referrers_unsupported: true,
            ..RecordingTransport::default()
        };
        let (result, calls, _temp, _signer) = drive_sign(mirrors, transport).await;
        result.expect("signing must succeed through a mirror on a fallback registry");

        assert!(
            calls.contains(&format!("push_manifest_raw:{}", fallback_tag_reference())),
            "the fallback index must be written to the canonical host, got: {calls:?}",
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("push_manifest_raw:mirror.example")),
            "no fallback write may reach the read-only mirror, got: {calls:?}",
        );
    }

    /// The other half: a registry that serves the Referrers API computes its own
    /// listing, so nothing writes the attacker-authorable tag beside it.
    #[tokio::test]
    async fn a_registry_with_the_referrers_api_gets_no_fallback_index() {
        let (calls, _state_dir) = run_recorded_sign(crate::oci::client::MirrorMap::default()).await;
        assert!(
            !calls.iter().any(|call| call.starts_with("push_manifest_raw:")),
            "a supported registry needs no fallback tag, got: {calls:?}",
        );
    }

    #[tokio::test]
    async fn sign_pushes_the_referrer_to_the_physical_registry_not_the_logical_one() {
        // Index indirection (`adr_index_indirection.md` C2): the logical
        // `ocx.sh/...` name is a pointer; the artifact lives on the physical
        // registry the index root names. A pipeline that builds its transport
        // reference from the LOGICAL identifier attaches the signature to a
        // host that does not hold the subject manifest — the signature is
        // written where no verifier will ever look for it.
        let (calls, _state_dir) = run_recorded_sign(crate::oci::client::MirrorMap::default()).await;
        assert!(
            calls.iter().any(|call| call == "push_referrer_manifest:8.8.8.8"),
            "the referrer manifest must be pushed to the physical registry, got: {calls:?}",
        );
        assert!(
            calls.iter().any(|call| call == "push_blob:8.8.8.8"),
            "the bundle blob must be pushed to the physical registry, got: {calls:?}",
        );
        assert!(
            calls.iter().all(|call| call.ends_with(":8.8.8.8")),
            "no transport call may target the logical index host, got: {calls:?}",
        );
    }

    #[tokio::test]
    async fn sign_reads_from_the_mirror_but_writes_to_the_canonical_host() {
        // ADR Q5: remote/proxy mirrors are read-only. The subject fetch is a
        // read and may come from the mirror; the capability probe and every
        // push must reach the canonical host, or signing breaks outright in a
        // mirrored deployment — and against a writable mirror it "succeeds"
        // while depositing the signature where no canonical verifier looks.
        let mirrors = crate::oci::client::MirrorMap::new([(
            "8.8.8.8".to_string(),
            crate::config::mirror::ParsedMirror {
                protocol: "https".to_string(),
                host: "mirror.example".to_string(),
                path_prefix: "proxy".to_string(),
            },
        )]);
        let (calls, state_dir) = run_recorded_sign(mirrors).await;

        assert!(
            calls.iter().any(|call| call == "pull_manifest_raw:mirror.example"),
            "the subject-manifest read must go to the mirror, got: {calls:?}",
        );
        for write in ["push_blob:8.8.8.8", "push_referrer_manifest:8.8.8.8"] {
            assert!(
                calls.iter().any(|call| call == write),
                "`{write}` must target the canonical host, got: {calls:?}",
            );
        }
        assert!(
            calls.iter().any(|call| call == "list_referrers:8.8.8.8"),
            "the capability probe must ask the host we push to, got: {calls:?}",
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("push_") && call.ends_with(":mirror.example")),
            "no write may reach the read-only mirror, got: {calls:?}",
        );

        // The capability record must be keyed on the host actually probed — the
        // canonical one for sign. `from_cache` returns None when the stored
        // `registry` disagrees with the lookup key, so these two pin the key.
        let state = StateStore::new(state_dir.path());
        assert!(
            ReferrersApiCapability::from_cache("8.8.8.8", &state)
                .await
                .expect("cache read")
                .is_some(),
            "sign must cache the capability under the canonical host it probed",
        );
        assert!(
            ReferrersApiCapability::from_cache("mirror.example", &state)
                .await
                .expect("cache read")
                .is_none(),
            "sign must not cache the canonical verdict under the mirror",
        );
    }

    #[tokio::test]
    async fn sign_refuses_a_subject_manifest_the_read_host_served_under_a_different_digest() {
        // The subject bytes come from the READ host (a different machine under
        // a `[mirrors]` entry) but their length becomes the descriptor `size`
        // pushed to the canonical host. A poisoned length yields a signature
        // strict verifiers reject, so the read must be bound to the resolved
        // digest and fail closed BEFORE anything is written.
        let transport = RecordingTransport {
            served_subject_digest: Some(crate::oci::Algorithm::Sha256.hash(b"a different manifest").to_string()),
            ..RecordingTransport::default()
        };
        let (result, calls, _state_dir, _signer) =
            drive_sign(crate::oci::client::MirrorMap::default(), transport).await;

        let Err(error) = result else {
            panic!("sign must refuse a subject manifest served under the wrong digest");
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
        assert!(
            !calls.iter().any(|call| call.starts_with("push_")),
            "nothing may be pushed after a subject-digest mismatch, got: {calls:?}",
        );
    }

    #[tokio::test]
    async fn sign_refuses_a_rewritten_registry_that_resolves_into_a_forbidden_range() {
        // CWE-918 at the dial site. The index root is remote data: a compromised
        // or hostile index can rewrite `ocx.sh/acme/tool` to point at a link-local
        // address, and the signing pipeline would then POST the bundle -- carrying
        // an OIDC-derived certificate -- at the cloud metadata endpoint. The
        // string-level check upstream (`ChainedIndex::guard_local_physical`)
        // TOLERATES a resolution failure by design; only the dial-site guard is
        // fail-closed, and until this call existed sign and verify never reached it.
        let (result, calls, _state, _signer) = drive_sign_at(
            "169.254.169.254/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
            RecordingTransport::default(),
            SignatureFormat::Bundle,
        )
        .await;
        let Err(error) = result else {
            panic!("a link-local rewrite target must be refused");
        };
        assert!(
            matches!(error.kind, SignErrorKind::ForbiddenRegistryTarget { .. }),
            "expected the SSRF refusal, got: {error}",
        );
        // The refusal must land BEFORE any traffic -- an error raised after the
        // first request has already leaked the credential it was meant to protect.
        assert!(
            calls.is_empty(),
            "no transport call may precede the refusal, got: {calls:?}",
        );
    }
}
