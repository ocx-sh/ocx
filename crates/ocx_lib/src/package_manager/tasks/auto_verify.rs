// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Policy-gated auto-verify hook fired at the metadata-first pull seam.
//!
//! [`PackageManager::maybe_auto_verify`] runs immediately after a package's
//! manifest is resolved (digest known) and before any layer download, so a
//! fail-closed abort leaves no package-store or symlink state. It sits in
//! `setup_impl`, the single choke point every package — root and transitive
//! dependency — passes through, so **every** install surface is gated: not just
//! `ocx package install` / `pull` but every `find_or_install` path (`package
//! exec`, `package env`, `run`, patch discovery). The config is attached once
//! on the shared manager in `Context::try_init`, so a new install command
//! inherits the gate for free.
//!
//! A failed covered install does leave the benign traces `resolve` already
//! wrote before the seam — the tag→digest pointer and manifest blobs committed
//! to the local index via write-through. These are inert (not usable installed
//! state; no package dir, no symlink); a re-resolve re-verifies before anything
//! is materialised.
//!
//! Gate (composes #98 `resolve_tiered` + #196 trust-root/offline + #194
//! pipeline via [`PackageManager::verify_one`]):
//!
//! 1. No [`AutoVerify`] configured (no trust policies) → no-op.
//! 2. A matching `[[trust.policy]]` covers the target → verify; a malformed
//!    matched policy is exit 78.
//! 3. No matching policy → INFO log, install proceeds (opt-in trust model).
//! 4. Covered + user opted out (`--no-verify` / `OCX_NO_VERIFY`) → WARN once
//!    per invocation, install proceeds.
//! 5. Covered + verify fails → abort fail-closed (exit code from the verify
//!    error taxonomy). Covered + verify passes → proceed.
//!
//! Trust-root material is resolved lazily (only when a policy actually covers a
//! package) and memoized, so a package outside every policy scope never trips
//! the offline gate.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::OnceCell;
use url::Url;

use crate::file_structure::StateStore;
use crate::oci::verify::trust_cache::cache_key_for_rekor;
use crate::oci::verify::{TrustRoot, VerifyError, VerifyErrorKind, resolve_trust_root};
use crate::oci::{self};
use crate::package_manager::error::PackageErrorKind;
use crate::trust::{self, TrustPolicy};

use super::super::PackageManager;
use super::verify::VerifyOptions;
use crate::oci::verify::VerifyContentMode;

/// Injected auto-verify configuration for the install/pull pipeline.
///
/// Present on a [`PackageManager`] only when at least one trust policy is
/// configured; absent → the hook is a no-op. Cheap to clone (the heavy
/// material is `Arc`-shared or resolved lazily).
#[derive(Clone)]
pub struct AutoVerify {
    /// Operator-tier policies from `config.toml` (authoritative).
    operator_policies: Arc<Vec<TrustPolicy>>,
    /// Project-tier policies from `ocx.toml` (empty for OCI-tier install/pull).
    project_policies: Arc<Vec<TrustPolicy>>,
    /// Registry client — its transport is used even under `--offline` (verify
    /// reads the artifact + signature referrer from the registry regardless).
    registry_client: oci::Client,
    /// Rekor transparency-log endpoint (default public Rekor).
    rekor_url: Url,
    /// Sigstore-trust-services offline flag.
    offline: bool,
    /// State store owning the capability + trust-root cache layouts.
    state: StateStore,
    /// `OCX_SIGSTORE_TRUSTED_ROOT` override captured at construction.
    trusted_root_env: Option<PathBuf>,
    /// Operator `[trust.sigstore]` from `config.toml`, captured at construction.
    sigstore_trust: Option<trust::SigstoreTrust>,
    /// `$OCX_HOME/sigstore/trusted-root.json` convention path, if `$OCX_HOME`
    /// resolved. Passed in rather than read here so the ladder stays free of
    /// environment reads and a test can point it anywhere.
    home_trusted_root: Option<PathBuf>,
    /// User opted out of verification (resolved `--no-verify` / `OCX_NO_VERIFY`,
    /// flag wins over env).
    user_opted_out: bool,
    /// Lazily-resolved trust root, memoized on success (`get_or_try_init`).
    trust_root: Arc<OnceCell<TrustRoot>>,
    /// WARN-once latch, shared across a batch install.
    warned: Arc<AtomicBool>,
}

/// Caller-provided inputs for [`AutoVerify::new`].
pub struct AutoVerifyInput {
    /// Operator-tier policies from `config.toml`.
    pub operator_policies: Vec<TrustPolicy>,
    /// Project-tier policies from `ocx.toml`.
    pub project_policies: Vec<TrustPolicy>,
    /// Registry client (present in every mode).
    pub registry_client: oci::Client,
    /// Rekor endpoint.
    pub rekor_url: Url,
    /// Sigstore-trust-services offline flag.
    pub offline: bool,
    /// State store owning the capability + trust-root cache layouts.
    pub state: StateStore,
    /// `OCX_SIGSTORE_TRUSTED_ROOT` override, if set.
    pub trusted_root_env: Option<PathBuf>,
    /// Operator `[trust.sigstore]` from `config.toml`, if configured.
    pub sigstore_trust: Option<trust::SigstoreTrust>,
    /// `$OCX_HOME/sigstore/trusted-root.json` convention path, if resolvable.
    pub home_trusted_root: Option<PathBuf>,
    /// Resolved user opt-out.
    pub user_opted_out: bool,
}

impl AutoVerify {
    /// Build an auto-verify config from resolved inputs.
    #[must_use]
    pub fn new(input: AutoVerifyInput) -> Self {
        Self {
            operator_policies: Arc::new(input.operator_policies),
            project_policies: Arc::new(input.project_policies),
            registry_client: input.registry_client,
            rekor_url: input.rekor_url,
            offline: input.offline,
            state: input.state,
            trusted_root_env: input.trusted_root_env,
            sigstore_trust: input.sigstore_trust,
            home_trusted_root: input.home_trusted_root,
            user_opted_out: input.user_opted_out,
            trust_root: Arc::new(OnceCell::new()),
            warned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Override the resolved user opt-out. The shared config is built with the
    /// `OCX_NO_VERIFY` env default; `ocx package install` / `pull` refine it
    /// from their `--verify` / `--no-verify` flag, which wins over the env.
    #[must_use]
    pub fn with_user_opted_out(mut self, opted_out: bool) -> Self {
        self.user_opted_out = opted_out;
        self
    }
}

impl PackageManager {
    /// Policy-gated auto-verify for a resolved package.
    ///
    /// A no-op when no [`AutoVerify`] is configured. See the module docs for the
    /// full gate. Called from the pull pipeline after resolve, before download.
    ///
    /// # Errors
    /// Returns a [`PackageErrorKind`] (fail-closed) when a policy-covered
    /// package fails verification, when a matched policy is malformed (exit 78),
    /// or when required trust material is unavailable (exit 78).
    ///
    /// `resolved` is the platform-selected leaf digest (`ResolvedChain.pinned`),
    /// so verification runs against `Platform::any()` — the leaf is already a
    /// flat manifest, and re-selecting it with the concrete platform would
    /// strict-equality-fail against the leaf's advertised `any()`.
    pub async fn maybe_auto_verify(&self, resolved: &oci::Identifier) -> Result<(), PackageErrorKind> {
        let Some(auto_verify) = self.auto_verify() else {
            return Ok(());
        };

        // Resolve the effective ANY-of policy set for this target under
        // cross-tier precedence (operator config.toml authoritative).
        let target = format!("{}/{}", resolved.registry(), resolved.repository());
        let policies = trust::resolve_tiered(&auto_verify.operator_policies, &auto_verify.project_policies, &target)
            .map_err(|source| verify_kind(resolved, VerifyErrorKind::TrustPolicyInvalid(source)))?;

        if policies.is_empty() {
            crate::log::info!(
                "no trust policy covers '{target}'; installing '{resolved}' without signature verification"
            );
            return Ok(());
        }

        // The package is policy-covered from here — every exit is verify or a
        // deliberate opt-out, never a silent skip.
        if auto_verify.user_opted_out {
            if !auto_verify.warned.swap(true, Ordering::Relaxed) {
                crate::log::warn!(
                    "signature verification skipped for policy-covered package(s) via --no-verify / OCX_NO_VERIFY"
                );
            }
            return Ok(());
        }

        // Trusted-hosts set for the target's registry — the same value the
        // pipeline's own SSRF floor reads (`VerifyPipeline::run_inner` step 0
        // calls `ctx.index.trusted_hosts_for(..)`). `verify_one` resolves through
        // `read_only_view`, which carries this set unchanged, so the guard below
        // and the pipeline's can never disagree about what is exempt.
        let trusted = self.index().trusted_hosts_for(resolved.registry());

        // Resolve the trust root lazily — only now that a policy matches, so a
        // package outside every scope never trips the offline gate. Memoized on
        // success; a failure recomputes (fail-closed) on the next covered package.
        let trust_root = auto_verify
            .trust_root
            .get_or_try_init(|| async {
                let root = resolve_trust_root(
                    auto_verify.trusted_root_env.as_deref(),
                    auto_verify.sigstore_trust.as_ref(),
                    auto_verify.home_trusted_root.as_deref(),
                    &auto_verify.state,
                    &cache_key_for_rekor(&auto_verify.rekor_url),
                    auto_verify.offline,
                )
                .await?;
                // Online with a bare Fulcio-CA PEM root (no pinned Rekor key):
                // fetch the Rekor key ONCE here and pin it into the memoized
                // root, so the N covered packages in a batch reuse it instead of
                // each TOFU-fetching it inside the pipeline.
                if !auto_verify.offline && root.rekor_public_key_pem().is_none() {
                    // Hoisting that fetch out of `verify_one` also hoisted it out
                    // of the pipeline's SSRF floor, which guards this exact dial
                    // on every other verify path. Re-apply the floor here, or the
                    // one Sigstore endpoint auto-verify dials on its own is the
                    // only one reached without a resolve-then-validate check
                    // (CWE-918).
                    crate::oci::endpoint::resolve_sigstore_url(&auto_verify.rekor_url, trusted)
                        .await
                        .map_err(|error| VerifyErrorKind::InvalidEndpointUrl {
                            endpoint: "Rekor endpoint".into(),
                            reason: crate::oci::endpoint::UrlRejection::from(error),
                        })?;
                    let key = crate::oci::verify::pipeline::fetch_rekor_public_key_pem(&auto_verify.rekor_url).await?;
                    root.with_rekor_key_pem(&key)
                } else {
                    Ok(root)
                }
            })
            .await
            .map_err(|kind| verify_kind(resolved, kind))?;

        let options = VerifyOptions {
            policies: &policies,
            client: &auto_verify.registry_client,
            trust_root,
            rekor_url: &auto_verify.rekor_url,
            offline: auto_verify.offline,
            state: &auto_verify.state,
            no_cache: false,
            // Auto-verify asks "is this artifact signed", never "what does it
            // carry": attestations do not participate (S-015). A subject whose
            // referrers are all attestations still fails closed here.
            content: VerifyContentMode::Signature,
        };
        self.verify_one(resolved, &oci::Platform::any(), options)
            .await
            .map_err(|error| error.kind)?;

        crate::log::debug!("auto-verify passed for '{resolved}'");
        Ok(())
    }
}

/// Wrap a [`VerifyErrorKind`] as a package-manager error preserving the verify
/// exit code (`Internal(crate::Error::Verify)` → `VerifyError::classify`).
fn verify_kind(identifier: &oci::Identifier, kind: VerifyErrorKind) -> PackageErrorKind {
    PackageErrorKind::Internal(crate::Error::Verify(Box::new(VerifyError::new(
        identifier.clone(),
        kind,
    ))))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use super::*;
    use crate::file_structure::FileStructure;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
    use crate::oci::verify::trust_cache::TrustRootCache;

    const REGISTRY: &str = "example.com";
    const REPO: &str = "widget";

    /// The cloud-metadata link-local address: forbidden by the SSRF floor, and
    /// an IP literal, so `lookup_host` answers from the literal itself — the
    /// guarded path opens no socket and needs no DNS.
    const FORBIDDEN_REKOR: &str = "https://169.254.169.254";

    fn covering_policy() -> TrustPolicy {
        TrustPolicy {
            scope: Some(crate::trust::ScopeSpec::Prefix(format!("{REGISTRY}/{REPO}"))),
            builder: None,
            keyless: Some(crate::trust::KeylessMatcher {
                identity: Some("you@example.com".into()),
                identity_regexp: None,
                oidc_issuer: Some("https://example.com".into()),
            }),
            system_locked: false,
        }
    }

    /// The batch-amortized Rekor-key fetch must pass the same SSRF floor the
    /// verify pipeline applies to that dial. Hoisting the fetch out of
    /// `verify_one` for the batch must not hoist it out of the guard.
    ///
    /// Discriminator: a link-local Rekor endpoint with no `trusted_hosts`
    /// exemption is refused as `InvalidEndpointUrl` before anything is dialed.
    /// Unguarded, the fetch is attempted and the failure is `TransparencyLogUnavailable` —
    /// a transport fault, reported as though the endpoint were legitimate.
    #[tokio::test]
    async fn hoisted_rekor_key_fetch_passes_the_ssrf_floor() {
        let root = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(root.path().to_path_buf());
        let state = StateStore::new(root.path().join("state"));
        let rekor_url = Url::parse(FORBIDDEN_REKOR).unwrap();

        // A keyless cache entry resolves the trust root with no network and
        // leaves `rekor_public_key_pem` unset — the one state that reaches the
        // hoisted fetch. Same fixture shape as `trust_resolve`'s own tests.
        TrustRootCache {
            rekor_authority: cache_key_for_rekor(&rekor_url),
            fulcio_der_certs: vec![vec![0x30, 0x00]],
            ctfe_keys: BTreeMap::new(),
            rekor_public_key_pem: None,
            cached_at: std::time::SystemTime::now(),
            ttl_seconds: 3600,
        }
        .write_cache(&state)
        .await
        .unwrap();

        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: file_structure.index.clone(),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        let auto_verify = AutoVerify::new(AutoVerifyInput {
            operator_policies: vec![covering_policy()],
            project_policies: Vec::new(),
            registry_client: oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new()))),
            rekor_url,
            offline: false,
            state,
            trusted_root_env: None,
            sigstore_trust: None,
            home_trusted_root: None,
            user_opted_out: false,
        });
        let manager = PackageManager::new(file_structure, index, None, REGISTRY).with_auto_verify(Some(auto_verify));

        let target = oci::Identifier::new_registry(REPO, REGISTRY).clone_with_tag("1.0");
        let error = manager
            .maybe_auto_verify(&target)
            .await
            .expect_err("a link-local Rekor endpoint must be refused, not dialed");

        match error {
            PackageErrorKind::Internal(crate::Error::Verify(verify_error)) => assert!(
                matches!(verify_error.kind, VerifyErrorKind::InvalidEndpointUrl { .. }),
                "the hoisted Rekor-key fetch must be refused by the SSRF floor, got {:?}",
                verify_error.kind
            ),
            other => panic!("expected Internal(Verify(InvalidEndpointUrl)), got {other:?}"),
        }
    }
}
