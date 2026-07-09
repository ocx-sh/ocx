// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package verify` — keyless Sigstore verification of a target manifest's
//! signature via OCI Referrers.
//!
//! Fetches the Sigstore bundle v0.3 referrer for the target, verifies the
//! Fulcio cert chain against the embedded TUF trust root, verifies the Rekor
//! SET, verifies the signature over the subject digest, and checks the cert
//! identity + issuer against user-supplied flags. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full state machine.
//!
//! There are **no default** `--certificate-identity` / `--certificate-oidc-issuer`
//! values — keyless verification is meaningless without knowing whose
//! signature you trust.
//!
//! This command resolves the identifier, validates `--rekor-url` (SSRF guard),
//! resolves the trust root in precedence order — `--tuf-root` /
//! `OCX_SIGSTORE_TUF_ROOT` (a trusted-root JSON with a pinned Rekor key), then
//! `--trust-root` / `OCX_SIGSTORE_TRUST_ROOT` (a Fulcio-CA PEM), then the fresh
//! trust-root cache, then the stubbed embedded root — and drives
//! [`VerifyPipeline::run`], which runs the full state machine and returns a
//! [`VerificationReport`].
//!
//! Verify reads the artifact and its signature referrer from the registry in
//! every mode. `--offline` / `OCX_OFFLINE` scopes to the Sigstore trust services
//! (the Rekor-key fetch and TUF), not the artifact registry: offline verify
//! reuses cached or supplied trust material (which must carry a pinned Rekor
//! key) and never contacts Sigstore; with no such material it fails with an
//! actionable error rather than skipping verification. A successful online
//! verify caches its trust material for later offline runs. The positive path is
//! currently exercised only against the fake Sigstore stack; production
//! hardening against public-good Fulcio/Rekor/TUF is tracked separately.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;
use ocx_lib::oci::endpoint::validate_sigstore_url;
use ocx_lib::oci::verify::{TrustRoot, TrustRootCache, VerifyContext, VerifyError, VerifyErrorKind, VerifyPipeline};
use ocx_lib::trust::{self, CompiledPolicy};

use crate::api::data::verification::VerificationReport;
use crate::options;

/// Default public Rekor transparency-log endpoint (overridable via `--rekor-url`).
const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";

#[derive(Parser, Clone)]
pub struct Verify {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    /// Expected certificate SAN (exact match).
    ///
    /// Optional when a `[trust.policy]` whose scope covers the target supplies
    /// the identity; when given, this flag and `--certificate-oidc-issuer`
    /// override any policy. The two flags are used together; supplying one
    /// without the other is an error.
    ///
    /// Example: `you@example.com`, `https://github.com/org/repo/.github/workflows/build.yml@refs/heads/main`.
    #[clap(
        long = "certificate-identity",
        value_name = "IDENTITY",
        requires = "certificate_oidc_issuer"
    )]
    certificate_identity: Option<String>,

    /// Expected certificate OIDC issuer (exact match).
    ///
    /// Optional when a matching `[trust.policy]` supplies the issuer; used
    /// together with `--certificate-identity` to override any policy.
    ///
    /// Example: `https://github.com/login/oauth`, `https://token.actions.githubusercontent.com`.
    #[clap(
        long = "certificate-oidc-issuer",
        value_name = "URL",
        requires = "certificate_identity"
    )]
    certificate_oidc_issuer: Option<String>,

    /// Rekor transparency-log endpoint (C-S1-3 injection seam, defaults to public Rekor).
    #[clap(long = "rekor-url", value_name = "URL", default_value = DEFAULT_REKOR_URL)]
    rekor_url: String,

    /// Bypass the referrers-capability cache for this invocation.
    #[clap(long = "no-cache")]
    no_cache: bool,

    /// Trust-root override: a PEM file of Fulcio CA certificate(s).
    ///
    /// Points verification at a custom Fulcio CA PEM for a private Sigstore
    /// instance. The Rekor public key is not in a PEM, so it is fetched from
    /// --rekor-url the first time; use --tuf-root to pin it (required offline).
    /// The flag takes precedence over the OCX_SIGSTORE_TRUST_ROOT env var.
    #[clap(long = "trust-root", value_name = "PATH")]
    trust_root: Option<std::path::PathBuf>,

    /// Trust-root override: a Sigstore trusted-root JSON (or a directory holding
    /// trusted_root.json).
    ///
    /// Supplies both the Fulcio CA and the pinned Rekor public key for
    /// air-gapped verification against a local trust-root mirror. No TUF network
    /// fetch is performed. Takes precedence over --trust-root and the
    /// OCX_SIGSTORE_TUF_ROOT env var (the flag wins). See
    /// https://ocx.sh/docs/in-depth/signing#offline-verification
    #[clap(long = "tuf-root", value_name = "PATH")]
    tuf_root: Option<std::path::PathBuf>,

    /// Package identifier to verify (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl Verify {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // SSRF hardening (CWE-918): validate user-supplied endpoint URL at the
        // boundary before it becomes an HTTP client target. Wrap the
        // UrlRejection into `VerifyErrorKind::InvalidEndpointUrl` so the
        // exit-code classifier maps it to `UsageError` (64) via the verify
        // error path — no cross-subsystem dependency on SignError.
        let rekor_url = validate_sigstore_url(&self.rekor_url, "--rekor-url").map_err(|reason| {
            VerifyError::new(
                identifier.clone(),
                VerifyErrorKind::InvalidEndpointUrl {
                    endpoint: "--rekor-url".into(),
                    reason,
                },
            )
        })?;

        // Verify reads the artifact + its signature referrer from the registry in
        // every mode. `--offline` scopes to the Sigstore trust services (the
        // Rekor-key fetch and TUF), not the registry — so, unlike sign, offline
        // verify does not exit 81; it requires cached/supplied trust material
        // instead. See `verify_context`.
        let (index, client, offline) = context.verify_context();

        // The trust-root cache is keyed by the Rekor instance; compute the key
        // here (where `rekor_url`'s type is in scope) so the resolver takes a
        // plain string and the CLI need not name `url::Url`.
        let rekor_cache_key = ocx_lib::oci::verify::trust_cache::cache_key_for_rekor(&rekor_url);
        let trust_root = self
            .resolve_trust_root(&identifier, context.file_structure().root(), &rekor_cache_key, offline)
            .await?;

        // Resolve the identity constraints: flag override (exact pair), or the
        // scope-matched [[trust.policy]] set pooled across config.toml tiers +
        // the project ocx.toml.
        let policies = self.resolve_policies(&context, &identifier).await?;

        let verify_context = VerifyContext {
            identifier: &identifier,
            platform: &self.platform,
            policies: &policies,
            no_cache: self.no_cache,
            transport: client.transport(),
            index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            cache_root: context.file_structure().root(),
            offline,
        };
        let result = VerifyPipeline::run(verify_context).await?;

        let report = VerificationReport::new(
            result.subject_digest,
            result.referrer_digest,
            result.certificate_identity,
            result.certificate_oidc_issuer,
            iso8601(result.signed_at),
        );
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }

    /// Build the ANY-of identity constraints the signing certificate must
    /// satisfy.
    ///
    /// Flag mode (`--certificate-identity` + `--certificate-oidc-issuer`, kept
    /// both-or-neither by clap): a single exact pair that overrides any policy
    /// — this preserves the original flag-only verify behaviour unchanged.
    /// Policy mode (neither flag): the scope-matched `[[trust.policy]]` set
    /// under cross-tier precedence — the operator `config.toml` tiers are
    /// authoritative; the project `ocx.toml` only adds trust where the operator
    /// has not governed the scope (see [`trust::resolve_tiered`]). A malformed
    /// matched policy → [`VerifyErrorKind::TrustPolicyInvalid`] (exit 78); no
    /// matching policy → [`VerifyErrorKind::NoIdentityProvided`] (exit 64).
    async fn resolve_policies(
        &self,
        context: &crate::app::Context,
        identifier: &oci::Identifier,
    ) -> anyhow::Result<Vec<CompiledPolicy>> {
        if let (Some(identity), Some(issuer)) = (&self.certificate_identity, &self.certificate_oidc_issuer) {
            return Ok(vec![CompiledPolicy::exact(identity.clone(), issuer.clone())]);
        }

        let target = format!("{}/{}", identifier.registry(), identifier.repository());
        let project_policies = self.project_trust_policies(context).await?;
        // Operator tier (config.toml) is authoritative; the project ocx.toml
        // only adds trust for scopes the operator has not governed.
        let compiled = trust::resolve_tiered(context.config_trust_policies(), &project_policies, &target)
            .map_err(|kind| VerifyError::new(identifier.clone(), VerifyErrorKind::from(kind)))?;
        if compiled.is_empty() {
            return Err(VerifyError::new(identifier.clone(), VerifyErrorKind::NoIdentityProvided).into());
        }
        Ok(compiled)
    }

    /// The project `ocx.toml` trust policies for the in-effect project (empty
    /// when no project file resolves). This is the deliberate OCI-tier carve-out
    /// for a security concern — verify reads `[[trust.policy]]` from `ocx.toml`,
    /// which OCI-tier commands otherwise never consult (see `adr_trust_policy.md`).
    async fn project_trust_policies(&self, context: &crate::app::Context) -> anyhow::Result<Vec<trust::TrustPolicy>> {
        let cwd = std::env::current_dir().ok();
        let ocx_home = context.file_structure().root();
        let resolved = ocx_lib::project::ProjectConfig::resolve(
            cwd.as_deref(),
            context.project_path(),
            Some(ocx_home),
            context.global(),
        )
        .await?;
        match resolved {
            Some((config_path, _lock_path)) => {
                // Lenient trust-only parse: an unrelated malformed section (a bad
                // `[tools]` entry, etc.) must NOT fail verify — only `[trust]`
                // matters here (the OCI-tier carve-out is scoped to trust policy).
                let text = tokio::fs::read_to_string(&config_path).await?;
                Ok(trust::policies_from_ocx_toml(&text)?)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Resolve the trust root in precedence order, offline-aware.
    ///
    /// 1. `--tuf-root` / `OCX_SIGSTORE_TUF_ROOT` — a Sigstore trusted-root JSON
    ///    (Fulcio CA + **pinned Rekor key**); the air-gapped seam, no TUF fetch.
    /// 2. `--trust-root` / `OCX_SIGSTORE_TRUST_ROOT` — a Fulcio-CA PEM (no Rekor
    ///    key; the pipeline fetches it online, or pins it from the cache).
    /// 3. The fresh trust-root cache for this Rekor instance (Fulcio + Rekor key),
    ///    populated by a prior online verify.
    /// 4. The embedded root (stubbed → exit 78).
    ///
    /// Offline additionally requires the resolved material to carry a pinned
    /// Rekor key (only 1 and 3 do). Offline + a bare PEM, or offline + an empty
    /// cache, fails with an actionable exit-78 error naming the remedy — verify
    /// is never silently skipped.
    async fn resolve_trust_root(
        &self,
        identifier: &oci::Identifier,
        cache_root: &std::path::Path,
        rekor_cache_key: &str,
        offline: bool,
    ) -> anyhow::Result<TrustRoot> {
        use ocx_lib::oci::verify::error::TrustRootLoadReason;

        let load_err = |kind: VerifyErrorKind| anyhow::Error::from(VerifyError::new(identifier.clone(), kind));
        let read_err = |source: std::io::Error| {
            load_err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::AssetReadFailed {
                source: Box::new(source),
            }))
        };

        // 1. TUF trusted-root JSON override (Fulcio CA + pinned Rekor key).
        let tuf_path = self
            .tuf_root
            .clone()
            .or_else(|| std::env::var_os("OCX_SIGSTORE_TUF_ROOT").map(std::path::PathBuf::from));
        if let Some(path) = tuf_path {
            let json_path = trusted_root_json_path(path);
            let bytes = std::fs::read(&json_path).map_err(read_err)?;
            let root = TrustRoot::load_trusted_root_json(&bytes).map_err(load_err)?;
            return self.enforce_offline_rekor_key(identifier, root, offline);
        }

        // 2. Fulcio-CA PEM override (no Rekor key).
        let pem_path = self
            .trust_root
            .clone()
            .or_else(|| std::env::var_os("OCX_SIGSTORE_TRUST_ROOT").map(std::path::PathBuf::from));
        if let Some(path) = pem_path {
            let bytes = std::fs::read(&path).map_err(read_err)?;
            let root = TrustRoot::load_from_pem(&bytes).map_err(load_err)?;
            return self.enforce_offline_rekor_key(identifier, root, offline);
        }

        // 3. Fresh trust-root cache for this Rekor instance (Fulcio + Rekor key).
        //    A normal cache entry always carries a Rekor key, but route it through
        //    the same offline gate so a hand-edited keyless entry still yields the
        //    actionable exit-78 error rather than a deeper exit-83.
        if let Ok(Some(cached)) = TrustRootCache::from_cache(rekor_cache_key, cache_root).await {
            return self.enforce_offline_rekor_key(identifier, cached.into_trust_root(), offline);
        }

        // 4. Nothing cached or supplied. Offline cannot fall back to the
        //    online-only embedded/fetch path — fail with the remedy.
        if offline {
            return Err(load_err(VerifyErrorKind::TrustRootLoad(
                TrustRootLoadReason::OfflineTrustMaterialUnavailable,
            )));
        }
        TrustRoot::load_embedded().map_err(load_err)
    }

    /// Offline verify needs a pinned Rekor key (the SET cannot be checked without
    /// one and there is no network to fetch it). A trust root that lacks one is
    /// an actionable exit-78 error offline; online it is fine (the key is
    /// fetched, then cached).
    fn enforce_offline_rekor_key(
        &self,
        identifier: &oci::Identifier,
        root: TrustRoot,
        offline: bool,
    ) -> anyhow::Result<TrustRoot> {
        use ocx_lib::oci::verify::error::TrustRootLoadReason;
        if offline && root.rekor_public_key_pem().is_none() {
            return Err(VerifyError::new(
                identifier.clone(),
                VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::OfflineTrustMaterialUnavailable),
            )
            .into());
        }
        Ok(root)
    }
}

/// Resolve a `--tuf-root` value to the trusted-root JSON file: the path itself
/// when it is a file, or `<dir>/trusted_root.json` when it names a directory.
fn trusted_root_json_path(path: std::path::PathBuf) -> std::path::PathBuf {
    if path.is_dir() {
        path.join("trusted_root.json")
    } else {
        path
    }
}

/// Format a UTC epoch-seconds timestamp as ISO-8601 (`YYYY-MM-DDThh:mm:ssZ`).
fn iso8601(epoch_secs: u64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs as i64, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
