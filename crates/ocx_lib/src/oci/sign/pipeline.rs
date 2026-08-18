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
//! [`OciTransport`]. No fallback `sha256-<digest>.sig` tag is ever written
//! (ADR S1-F) — signatures are OCI 1.1 referrers only.

use url::Url;

use super::error::{SignError, SignErrorKind};
use super::oidc::TokenProvider;
use super::signer::Signer;
use crate::file_structure::StateStore;
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport};
use crate::oci::index::{Index, IndexOperation, SelectResult};
use crate::oci::referrer::ReferrerManifest;
use crate::oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use crate::oci::referrer::media_types::{EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_PAYLOAD, SIGSTORE_BUNDLE_V03};
use crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE;
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
    /// Platform selector for multi-platform manifests.
    pub platform: &'a Platform,
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
}

/// Result emitted by a successful sign pipeline run.
pub struct SignResult {
    /// Digest of the target manifest the signature was attached to.
    pub subject_digest: Digest,
    /// Digest of the pushed Sigstore bundle blob.
    pub bundle_digest: Digest,
    /// Digest of the pushed referrer manifest.
    pub referrer_digest: Digest,
    /// Full OCI descriptor of the pushed referrer manifest.
    pub referrer_descriptor: Descriptor,
    /// Cert SAN (identity) that signed the target — the OIDC subject.
    pub certificate_identity: String,
    /// Cert issuer (`--certificate-oidc-issuer` comparand) — the OIDC issuer.
    pub certificate_oidc_issuer: String,
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
        //    they actually resolve, before anything dials them. Sign always
        //    reaches Fulcio and Rekor, so both are guarded unconditionally.
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
        // 1. Resolve the per-platform target manifest.
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
        Self::ensure_referrers_supported(transport, &ctx, &write_image, &subject_digest).await?;

        // 3. Acquire the OIDC token.
        let token = ctx.token_provider.acquire("sigstore").await?;

        // 4. Produce the Sigstore bundle.
        let bundle = ctx
            .signer
            .sign(&subject_digest, &token, ctx.fulcio_url, ctx.rekor_url)
            .await?;

        // 5. Push the referrer's blobs: the OCI empty-config blob (the manifest's
        //    `config` descriptor points at it) and the Sigstore bundle blob (the
        //    `layers[0]` payload). A spec-strict registry (zot) rejects the
        //    manifest with MANIFEST_INVALID if either referenced blob is absent,
        //    so both must land before the manifest PUT. `push_blob` HEADs first,
        //    so re-pushing the shared empty-config blob is a no-op after the first.
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

        // 6. Build + push the referrer manifest (subject → target).
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
        let manifest = ReferrerManifest::build(subject_descriptor, SIGSTORE_BUNDLE_V03, bundle_descriptor);
        let manifest_bytes = manifest.to_canonical_json()?;
        let referrer_descriptor = transport
            .push_referrer_manifest(&write_image, &subject_digest, &manifest_bytes, OCI_IMAGE_MEDIA_TYPE)
            .await
            .map_err(map_client_error)?;
        let referrer_digest =
            Digest::try_from(referrer_descriptor.digest.as_str()).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        Ok(SignResult {
            subject_digest,
            bundle_digest: bundle.digest,
            referrer_digest,
            referrer_descriptor,
            certificate_identity: bundle.certificate_identity,
            certificate_oidc_issuer: bundle.certificate_oidc_issuer,
        })
    }

    /// Confirm the registry serves the OCI Referrers API, consulting (and
    /// refreshing) the per-registry capability cache. `Unsupported` →
    /// [`SignErrorKind::ReferrersUnsupported`] (exit 84).
    async fn ensure_referrers_supported(
        transport: &dyn OciTransport,
        ctx: &SignContext<'_>,
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
                // Best-effort cache write; a failure here must not fail the sign.
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
fn map_client_error(error: ClientError) -> SignErrorKind {
    match error {
        ClientError::ReferrersUnsupported { .. } => SignErrorKind::ReferrersUnsupported,
        // Everything else keeps the `ClientError` intact under `Internal` rather
        // than being flattened into a sign-side kind that would misdescribe it:
        // the sign taxonomy's only exit-65 kind is `RekorSetMalformed`, whose
        // message ("Rekor SET malformed or missing") would blame the transparency
        // log for a registry-side malformed image index. Keeping the cause is not
        // a loss of the exit code -- `SignError::classify` defers on `Internal`,
        // so the wrapped `ClientError` supplies its own (401 -> 80, 5xx -> 69,
        // transient -> 75, malformed data -> 65) while the sign-side `kind_detail`
        // honestly stays `internal`.
        other => SignErrorKind::Internal(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ExitCode, classify_error};

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
            _: &native::Reference,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("sign pushes the referrer manifest through push_referrer_manifest")
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

        async fn push_referrer_manifest(
            &self,
            image: &native::Reference,
            _: &Digest,
            manifest_bytes: &[u8],
            media_type: &str,
        ) -> std::result::Result<Descriptor, ClientError> {
            self.record("push_referrer_manifest", image);
            Ok(Descriptor {
                media_type: media_type.to_string(),
                digest: crate::oci::Algorithm::Sha256.hash(manifest_bytes).to_string(),
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
            // A successful (empty) listing is what the capability probe reads as
            // "this registry supports the Referrers API".
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

    struct FixedTokenProvider;

    #[async_trait::async_trait]
    impl TokenProvider for FixedTokenProvider {
        async fn acquire(&self, _: &str) -> Result<super::super::oidc::OidcToken, SignErrorKind> {
            Ok(super::super::oidc::OidcToken::new(jwt_with_payload(
                &serde_json::json!({ "sub": "me@example.com", "iss": "https://issuer.example" }),
            )))
        }
    }

    struct FixedSigner;

    #[async_trait::async_trait]
    impl Signer for FixedSigner {
        async fn sign(
            &self,
            _: &Digest,
            _: &super::super::oidc::OidcToken,
            _: &Url,
            _: &Url,
        ) -> Result<crate::oci::sign::bundle::SignedBundle, SignErrorKind> {
            let bytes = br#"{"mediaType":"test-bundle"}"#.to_vec();
            let digest = crate::oci::Algorithm::Sha256.hash(&bytes);
            Ok(crate::oci::sign::bundle::SignedBundle {
                bytes,
                digest,
                certificate_identity: "me@example.com".to_string(),
                certificate_oidc_issuer: "https://issuer.example".to_string(),
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
        let (result, calls, temp) = drive_sign(mirrors, RecordingTransport::default()).await;
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
    ) -> (Result<SignResult, SignError>, Vec<String>, tempfile::TempDir) {
        // A public IP literal, not a name: the pipeline now resolves the physical
        // host before dialing it (dial-site SSRF guard), and an IP literal resolves
        // locally -- a DNS name here would make this unit test open a socket.
        drive_sign_at("8.8.8.8/acme/tool:1.0", mirrors, transport).await
    }

    /// `drive_sign` with the physical registry the index rewrites to made an
    /// argument, so a test can point the indirection at a forbidden target.
    async fn drive_sign_at(
        physical: &str,
        mirrors: crate::oci::client::MirrorMap,
        transport: RecordingTransport,
    ) -> (Result<SignResult, SignError>, Vec<String>, tempfile::TempDir) {
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
        let platform = Platform::any();
        let signer = FixedSigner;
        let token_provider = FixedTokenProvider;

        let result = SignPipeline::run(
            &client,
            SignContext {
                identifier: &logical,
                platform: &platform,
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
        (result, transport.calls(), temp)
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
        let (result, calls, _state_dir) = drive_sign(crate::oci::client::MirrorMap::default(), transport).await;

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
        let (result, calls, _state) = drive_sign_at(
            "169.254.169.254/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
            RecordingTransport::default(),
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
