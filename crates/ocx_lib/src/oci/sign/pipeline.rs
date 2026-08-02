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
                return Err(SignErrorKind::Internal(
                    format!("no manifest for {} on {}", ctx.identifier, ctx.platform).into(),
                ));
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
        let (subject_bytes, _) = transport
            .pull_manifest_raw(&subject_ref, ACCEPTED_MANIFEST_TYPES)
            .await
            .map_err(map_client_error)?;

        // 2. Referrers-API capability (cache-first) of the host we will PUSH
        //    to. A mirror's referrers support says nothing about the upstream's,
        //    and the upstream is where the referrer manifest has to land.
        Self::ensure_referrers_supported(transport, &ctx, &write_image, &subject_digest).await?;

        // 3. Acquire the OIDC token.
        let token = ctx.token_provider.acquire("sigstore").await?;
        let certificate_identity = jwt_claim(token.as_str(), "sub")
            .or_else(|| jwt_claim(token.as_str(), "email"))
            .unwrap_or_default();
        let certificate_oidc_issuer = jwt_claim(token.as_str(), "iss").unwrap_or_default();

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
            certificate_identity,
            certificate_oidc_issuer,
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
        // Explicit, and deliberately the same outcome as the catch-all: the
        // sign taxonomy's only exit-65 kind is `RekorSetMalformed`, whose
        // message ("Rekor SET malformed or missing") would misattribute a
        // registry-side malformed image index to the transparency log. A
        // truthful exit 65 here needs a new `SignErrorKind` variant (which
        // also changes the frozen `kind_detail` contract, C-S1-1); until then
        // an honest exit 1 beats a false diagnostic. `InvalidManifest` and
        // `DigestMismatch` — the sibling data-shaped client errors — fall
        // through the same way.
        other @ ClientError::InvalidImageIndex(_) => SignErrorKind::Internal(Box::new(other)),
        other => SignErrorKind::Internal(Box::new(other)),
    }
}

/// Read a string claim from a JWT without verifying it (the values only feed
/// the sign result's reporting fields; Fulcio is the authority on identity).
fn jwt_claim(jwt: &str, claim: &str) -> Option<String> {
    use base64::Engine as _;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get(claim).and_then(|v| v.as_str()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an unsigned JWT (`header.payload.sig`) whose payload is `claims`.
    fn jwt_with_payload(claims: &serde_json::Value) -> String {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("eyJhbGciOiJFUzI1NiJ9.{payload}.sig")
    }

    #[test]
    fn jwt_claim_reads_string_claims() {
        let jwt = jwt_with_payload(&serde_json::json!({
            "sub": "me@example.com",
            "iss": "https://issuer.example",
        }));
        assert_eq!(jwt_claim(&jwt, "sub").as_deref(), Some("me@example.com"));
        assert_eq!(jwt_claim(&jwt, "iss").as_deref(), Some("https://issuer.example"));
    }

    #[test]
    fn jwt_claim_is_none_for_missing_or_non_string_claims() {
        let jwt = jwt_with_payload(&serde_json::json!({ "sub": "me", "exp": 12345 }));
        assert_eq!(jwt_claim(&jwt, "email"), None, "absent claim");
        assert_eq!(jwt_claim(&jwt, "exp"), None, "numeric claim is not a string");
    }

    #[test]
    fn jwt_claim_is_none_for_undecodable_input() {
        assert_eq!(jwt_claim("not-a-jwt", "sub"), None, "no payload segment");
        assert_eq!(jwt_claim("h.!!!not-base64!!!.s", "sub"), None, "bad base64 payload");
        assert_eq!(jwt_claim("h..s", "sub"), None, "empty payload");
    }

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
    fn map_client_error_keeps_invalid_image_index_internal_alongside_its_siblings() {
        // Pins the deliberate choice behind the explicit arm: the sign taxonomy
        // has no exit-65 kind whose message fits a registry-side malformed image
        // index (`RekorSetMalformed` would blame the transparency log), so this
        // stays with `InvalidManifest` and `DigestMismatch` on the honest exit 1
        // until a dedicated variant exists.
        let index: crate::oci::ImageIndex =
            serde_json::from_slice(br#"{"schemaVersion":1,"manifests":[]}"#).expect("image index json");
        let invalid = crate::oci::manifest::validate_image_index(&index).expect_err("schemaVersion 1 is refused");
        assert!(matches!(
            map_client_error(ClientError::InvalidImageIndex(invalid)),
            SignErrorKind::Internal(_)
        ));
    }

    // ── Index indirection: transport traffic follows the PHYSICAL registry ──

    /// SHA-256 the indirecting test index reports as the subject digest.
    fn indirection_subject_digest() -> Digest {
        crate::oci::Algorithm::Sha256.hash(b"indirected subject manifest")
    }

    /// A test index whose logical name resolves to a DIFFERENT physical
    /// registry — the `index.ocx.sh` shape (`ocx.sh/<ns>/<pkg>` pointing at
    /// `oci://ghcr.io/<org>/<repo>`) reduced to what the pipeline consumes.
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
            Ok((b"{}".to_vec(), indirection_subject_digest().to_string()))
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
            Ok(crate::oci::sign::bundle::SignedBundle { bytes, digest })
        }

        fn signer_kind(&self) -> &'static str {
            "test-fixed"
        }
    }

    /// Drive a full sign run against the recording transport for the logical
    /// name `ocx.sh/acme/tool:1.0`, indirected to the physical
    /// `ghcr.io/acme/tool:1.0`. Returns the `"<method>:<registry>"` log plus the
    /// state dir, so a caller can read back the persisted capability record.
    /// Panics if the run does not complete.
    async fn run_recorded_sign(mirrors: crate::oci::client::MirrorMap) -> (Vec<String>, tempfile::TempDir) {
        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let physical = Identifier::parse("ghcr.io/acme/tool:1.0").expect("physical identifier");

        let transport = RecordingTransport::default();
        let mut client = Client::with_transport(Box::new(transport.clone()));
        client.mirrors = mirrors;
        let index = Index::from_impl(IndirectingIndex { physical });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let fulcio_url = Url::parse("https://fulcio.example").expect("fulcio url");
        let rekor_url = Url::parse("https://rekor.example").expect("rekor url");
        let platform = Platform::any();
        let signer = FixedSigner;
        let token_provider = FixedTokenProvider;

        if let Err(error) = SignPipeline::run(
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
        .await
        {
            panic!("sign must complete against the recording transport: {error}");
        }
        (transport.calls(), temp)
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
            calls.iter().any(|call| call == "push_referrer_manifest:ghcr.io"),
            "the referrer manifest must be pushed to the physical registry, got: {calls:?}",
        );
        assert!(
            calls.iter().any(|call| call == "push_blob:ghcr.io"),
            "the bundle blob must be pushed to the physical registry, got: {calls:?}",
        );
        assert!(
            calls.iter().all(|call| call.ends_with(":ghcr.io")),
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
            "ghcr.io".to_string(),
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
        for write in ["push_blob:ghcr.io", "push_referrer_manifest:ghcr.io"] {
            assert!(
                calls.iter().any(|call| call == write),
                "`{write}` must target the canonical host, got: {calls:?}",
            );
        }
        assert!(
            calls.iter().any(|call| call == "list_referrers:ghcr.io"),
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
            ReferrersApiCapability::from_cache("ghcr.io", &state)
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
}
