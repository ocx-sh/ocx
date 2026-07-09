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

use crate::oci::verify::trust_cache::cache_key_for_rekor;
use crate::oci::verify::{TrustRoot, VerifyError, VerifyErrorKind, resolve_trust_root};
use crate::oci::{self};
use crate::package_manager::error::PackageErrorKind;
use crate::trust::{self, TrustPolicy};

use super::super::PackageManager;
use super::verify::VerifyOptions;

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
    /// `$OCX_HOME` root for the capability + trust-root caches.
    cache_root: PathBuf,
    /// `OCX_SIGSTORE_TUF_ROOT` override captured at construction.
    tuf_root_env: Option<PathBuf>,
    /// `OCX_SIGSTORE_TRUST_ROOT` override captured at construction.
    pem_root_env: Option<PathBuf>,
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
    /// `$OCX_HOME` root.
    pub cache_root: PathBuf,
    /// `OCX_SIGSTORE_TUF_ROOT` override, if set.
    pub tuf_root_env: Option<PathBuf>,
    /// `OCX_SIGSTORE_TRUST_ROOT` override, if set.
    pub pem_root_env: Option<PathBuf>,
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
            cache_root: input.cache_root,
            tuf_root_env: input.tuf_root_env,
            pem_root_env: input.pem_root_env,
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

        // Resolve the trust root lazily — only now that a policy matches, so a
        // package outside every scope never trips the offline gate. Memoized on
        // success; a failure recomputes (fail-closed) on the next covered package.
        let trust_root = auto_verify
            .trust_root
            .get_or_try_init(|| async {
                resolve_trust_root(
                    auto_verify.tuf_root_env.as_deref(),
                    auto_verify.pem_root_env.as_deref(),
                    &auto_verify.cache_root,
                    &cache_key_for_rekor(&auto_verify.rekor_url),
                    auto_verify.offline,
                )
                .await
            })
            .await
            .map_err(|kind| verify_kind(resolved, kind))?;

        let options = VerifyOptions {
            policies: &policies,
            transport: auto_verify.registry_client.transport(),
            trust_root,
            rekor_url: &auto_verify.rekor_url,
            offline: auto_verify.offline,
            cache_root: &auto_verify.cache_root,
            no_cache: false,
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
