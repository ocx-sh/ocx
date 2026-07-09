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

use std::path::Path;

use url::Url;

use super::error::{SignError, SignErrorKind};
use super::oidc::TokenProvider;
use super::signer::Signer;
use crate::oci::client::OciTransport;
use crate::oci::client::error::ClientError;
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
    /// Registry transport.
    pub transport: &'a dyn OciTransport,
    /// Index for resolving tag → per-platform manifest digest.
    pub index: &'a Index,
    /// Fulcio URL (validated at the CLI boundary).
    pub fulcio_url: &'a Url,
    /// Rekor URL (validated at the CLI boundary).
    pub rekor_url: &'a Url,
    /// `$OCX_HOME` root for the referrers-capability cache.
    pub cache_root: &'a Path,
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
    pub async fn run(ctx: SignContext<'_>) -> Result<SignResult, SignError> {
        let identifier = ctx.identifier.clone();
        Self::run_inner(ctx)
            .await
            .map_err(|kind| SignError::new(identifier, kind))
    }

    async fn run_inner(ctx: SignContext<'_>) -> Result<SignResult, SignErrorKind> {
        // 1. Resolve the per-platform target manifest.
        let resolved = match ctx
            .index
            .select(ctx.identifier, vec![ctx.platform.clone()], IndexOperation::Resolve)
            .await
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?
        {
            SelectResult::Found(id) => id,
            SelectResult::Ambiguous(_) | SelectResult::NotFound => {
                return Err(SignErrorKind::Internal(
                    format!("no manifest for {} on {}", ctx.identifier, ctx.platform).into(),
                ));
            }
        };
        let subject_digest = resolved
            .digest()
            .ok_or_else(|| SignErrorKind::Internal("resolved target has no digest".into()))?;
        let registry = resolved.registry().to_string();
        let repo = resolved.repository().to_string();
        let image = native::Reference::with_tag(registry.clone(), repo.clone(), "latest".to_string());

        // Fetch the target manifest bytes for the subject descriptor's size.
        let subject_ref = native::Reference::with_digest(registry.clone(), repo.clone(), subject_digest.to_string());
        let (subject_bytes, _) = ctx
            .transport
            .pull_manifest_raw(&subject_ref, ACCEPTED_MANIFEST_TYPES)
            .await
            .map_err(map_client_error)?;

        // 2. Referrers-API capability (cache-first).
        Self::ensure_referrers_supported(&ctx, &registry, &repo, &subject_digest).await?;

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
        ctx.transport
            .push_blob(
                &image,
                EMPTY_CONFIG_PAYLOAD.to_vec(),
                &empty_config_digest,
                no_progress.clone(),
            )
            .await
            .map_err(map_client_error)?;
        ctx.transport
            .push_blob(&image, bundle.bytes.clone(), &bundle.digest, no_progress)
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
        let referrer_descriptor = ctx
            .transport
            .push_referrer_manifest(&image, &subject_digest, &manifest_bytes, OCI_IMAGE_MEDIA_TYPE)
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
        ctx: &SignContext<'_>,
        registry: &str,
        repo: &str,
        subject_digest: &Digest,
    ) -> Result<(), SignErrorKind> {
        let cached = if ctx.no_cache {
            None
        } else {
            ReferrersApiCapability::from_cache(registry, ctx.cache_root)
                .await
                .ok()
                .flatten()
                .filter(ReferrersApiCapability::is_fresh)
        };
        let capability = match cached {
            Some(hit) => hit,
            None => {
                let probed = ReferrersApiCapability::probe(ctx.transport, registry, repo, subject_digest)
                    .await
                    .map_err(map_client_error)?;
                // Best-effort cache write; a failure here must not fail the sign.
                let _ = probed.write_cache(ctx.cache_root).await;
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
}
