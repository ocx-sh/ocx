// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Policy-gated auto-verify hook fired at the metadata-first pull seam.
//!
//! Runs after resolve and before any layer download, so a fail-closed abort leaves no package or symlink state.
//! Called only from `setup_impl`: an install path that bypasses it installs unverified.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ocx_config::Config;
use tokio::sync::OnceCell;
use url::Url;

use crate::error::PackageErrorKind;
use ocx_oci::{self};
use ocx_sign::sign::state::SigningStatePaths;
use ocx_sign::verify::trust_cache::cache_key_for_rekor;
use ocx_sign::verify::{
    Transparency, TrustRoot, VerifyError, VerifyErrorKind, resolve_local_trust_root, resolve_trust_root,
};
use ocx_store::file_structure::StateStore;
use ocx_trust::{CompiledPolicy, TrustPolicy};

use super::super::PackageManager;
use super::signer_rounds::{
    RoundsDecision, RoundsError, SignerRounds, SignerRoundsInput, note_keyless_skipped, publisher_round_error,
    signer_rounds, signer_rounds_for_pinned,
};
use super::verify::VerifyOptions;
use ocx_sign::verify::VerifyContentMode;

/// Auto-verify configuration, present on a [`PackageManager`] only when a trust policy is configured.
#[derive(Clone)]
pub struct AutoVerify {
    /// Operator-tier policies from `config.toml` (authoritative).
    operator_policies: Arc<Vec<TrustPolicy>>,
    /// Project-tier policies from `ocx.toml` (empty for OCI-tier install/pull).
    project_policies: Arc<Vec<TrustPolicy>>,
    /// The merged `config.toml` tiers: the per-registry `signatures` setting.
    config: Arc<Config>,
    /// Registry client, used even under `--offline`: verify reads the artifact and its signature referrer.
    registry_client: ocx_oci::Client,
    /// Rekor transparency-log endpoint (default public Rekor).
    rekor_url: Url,
    /// Sigstore-trust-services offline flag.
    offline: bool,
    /// State store owning the capability + trust-root cache layouts.
    state: StateStore,
    /// `OCX_SIGSTORE_TRUSTED_ROOT` override captured at construction.
    trusted_root_env: Option<PathBuf>,
    /// Operator `[trust.sigstore]` from `config.toml`, captured at construction.
    sigstore_trust: Option<ocx_trust::SigstoreTrust>,
    /// `$OCX_HOME/sigstore/trusted-root.json` convention path, if `$OCX_HOME` resolved.
    home_trusted_root: Option<PathBuf>,
    /// Resolved `--no-verify` / `OCX_NO_VERIFY` opt-out (flag wins).
    user_opted_out: bool,
    /// Lazily-resolved full trust root (may reach TUF), memoized on success (`get_or_try_init`).
    trust_root: Arc<OnceCell<TrustRoot>>,
    /// The trust root already local with no network, memoized per batch; an empty root, which can check nothing,
    /// when no rung holds one.
    local_trust_root: Arc<OnceCell<TrustRoot>>,
    /// WARN-once latch, shared across a batch install.
    warned: Arc<AtomicBool>,
    /// Whether each logical repository declares publishers, memoized per batch for the origin gate.
    declarations: Arc<Mutex<HashMap<DeclarationKey, Arc<OnceCell<bool>>>>>,
}

/// `(registry, repository, committed_only)`: a digest request reads the committed root alone, a tag request the effective list.
type DeclarationKey = (String, String, bool);

/// Caller-provided inputs for [`AutoVerify::new`].
pub struct AutoVerifyInput {
    /// Operator-tier policies from `config.toml`.
    pub operator_policies: Vec<TrustPolicy>,
    /// Project-tier policies from `ocx.toml`.
    pub project_policies: Vec<TrustPolicy>,
    /// The merged `config.toml` tiers.
    pub config: Config,
    /// Registry client (present in every mode).
    pub registry_client: ocx_oci::Client,
    /// Rekor endpoint.
    pub rekor_url: Url,
    /// Sigstore-trust-services offline flag.
    pub offline: bool,
    /// State store owning the capability + trust-root cache layouts.
    pub state: StateStore,
    /// `OCX_SIGSTORE_TRUSTED_ROOT` override, if set.
    pub trusted_root_env: Option<PathBuf>,
    /// Operator `[trust.sigstore]` from `config.toml`, if configured.
    pub sigstore_trust: Option<ocx_trust::SigstoreTrust>,
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
            config: Arc::new(input.config),
            registry_client: input.registry_client,
            rekor_url: input.rekor_url,
            offline: input.offline,
            state: input.state,
            trusted_root_env: input.trusted_root_env,
            sigstore_trust: input.sigstore_trust,
            home_trusted_root: input.home_trusted_root,
            user_opted_out: input.user_opted_out,
            trust_root: Arc::new(OnceCell::new()),
            local_trust_root: Arc::new(OnceCell::new()),
            warned: Arc::new(AtomicBool::new(false)),
            declarations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Whether `logical` declares publishers, read once per batch: every `find` consults it, and a declaration
    /// read parses the whole committed root.
    ///
    /// # Errors
    /// An unreadable committed root fails closed rather than reading as undeclared.
    async fn declares_publishers(
        &self,
        index: &ocx_index::Index,
        logical: &ocx_oci::PackageRef,
        committed_only: bool,
    ) -> Result<bool, PackageErrorKind> {
        let key = (
            logical.registry().to_ascii_lowercase(),
            logical.repository().to_owned(),
            committed_only,
        );
        let cell = self
            .declarations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key)
            .or_default()
            .clone();
        cell.get_or_try_init(|| async {
            let declared = if committed_only {
                index.committed_publishers(logical).await
            } else {
                index.declared_publishers(logical).await
            }
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
            Ok(!matches!(declared, ocx_index::DeclaredPublishers::Undeclared))
        })
        .await
        .copied()
    }

    /// Refuse the opt-out where the system config locks `signatures` to `required` for `registry`: an operator
    /// decision no flag or environment variable may lift.
    fn refuse_locked_opt_out(&self, registry: &str) -> Result<(), RoundsError> {
        if self.user_opted_out
            && self.config.signature_policy(registry) == (ocx_config::SignatureRequirement::Required, true)
        {
            return Err(RoundsError::NoVerifyRefused {
                registry: registry.to_owned(),
                config_path: ocx_config::loader::ConfigLoader::system_path().display().to_string(),
            });
        }
        Ok(())
    }

    /// Override the `OCX_NO_VERIFY`-derived opt-out with a `--verify` / `--no-verify` flag, which wins.
    #[must_use]
    pub fn with_user_opted_out(mut self, opted_out: bool) -> Self {
        self.user_opted_out = opted_out;
        self
    }
}

impl PackageManager {
    /// Whether already-installed content may be handed out without a pull.
    ///
    /// A package its index declares publishers for counts as installed only when `refs/verified/` holds a marker
    /// for its logical repository: written only after a publisher round passed, so content installed while
    /// undeclared, opted out or from a lock without a committed root is verified before it is served under a
    /// declared name. Undeclared packages, a `signatures = "off"` registry, `--no-verify` and a manager with no
    /// [`AutoVerify`] keep the plain store-hit fast path.
    ///
    /// `committed_only` is whether the request named a digest, which reads the committed root alone as the
    /// install-side rounds do (`signer_rounds_for_pinned`).
    pub(crate) async fn origin_gate_open(
        &self,
        logical: &ocx_oci::PackageRef,
        info: &ocx_package::install_info::InstallInfo,
        committed_only: bool,
    ) -> Result<bool, PackageErrorKind> {
        let Some(auto_verify) = self.auto_verify() else {
            return Ok(true);
        };
        auto_verify
            .refuse_locked_opt_out(logical.registry())
            .map_err(|refusal| PackageErrorKind::Internal(refusal.into()))?;
        if auto_verify.user_opted_out
            || auto_verify.config.signature_requirement(logical.registry()) == ocx_config::SignatureRequirement::Off
        {
            return Ok(true);
        }
        if !auto_verify
            .declares_publishers(self.index(), logical, committed_only)
            .await?
        {
            return Ok(true);
        }
        let open = ocx_store::file_structure::is_verified(info.dir(), logical).await;
        if !open {
            log::debug!(
                "No verified marker for '{}/{}' on a declared package; falling through to the pull.",
                logical.registry(),
                logical.repository()
            );
        }
        Ok(open)
    }

    /// Auto-verify for a resolved package: the index-declared publisher round, then the operator-policy round;
    /// a no-op when no [`AutoVerify`] is configured.
    ///
    /// `resolved` must be the platform-selected leaf (`ResolvedChain.pinned`): verification does not narrow.
    ///
    /// # Errors
    /// Returns a [`PackageErrorKind`] (fail-closed) when a covered package fails either round, when the
    /// publishers refuse it (a `RoundsError`, unless opted out), when a matched policy is malformed (exit 78), or
    /// when required trust material is unavailable (exit 78, or 81 offline with no pinned Rekor key).
    pub async fn maybe_auto_verify(&self, resolved: &ocx_oci::PackageRef) -> Result<(), PackageErrorKind> {
        self.maybe_auto_verify_with(resolved, false).await.map(drop)
    }

    /// [`Self::maybe_auto_verify`] for a package installed on `requested`: a request that names a digest (a digest
    /// install or an `ocx.lock` pin) asks the index for no declaration, so only the committed root declares
    /// Publishers.
    ///
    /// Returns whether a publisher round ran and passed, the one fact the verified marker may be written on.
    ///
    /// # Errors
    /// As [`Self::maybe_auto_verify`].
    pub async fn maybe_auto_verify_requested(
        &self,
        resolved: &ocx_oci::PackageRef,
        requested: &ocx_oci::PackageRef,
    ) -> Result<bool, PackageErrorKind> {
        self.maybe_auto_verify_with(resolved, requested.digest().is_some())
            .await
    }

    async fn maybe_auto_verify_with(
        &self,
        resolved: &ocx_oci::PackageRef,
        committed_only: bool,
    ) -> Result<bool, PackageErrorKind> {
        let Some(auto_verify) = self.auto_verify() else {
            return Ok(false);
        };
        // Boxed: the rounds' state would otherwise widen every install command's future and, through the root
        // dispatch, overflow a debug test thread's stack.
        Box::pin(self.auto_verify_rounds(auto_verify, resolved, committed_only)).await
    }

    fn warn_skipped_once(&self, auto_verify: &AutoVerify) {
        if !auto_verify.warned.swap(true, Ordering::Relaxed) {
            log::warn!("signature verification skipped for policy-covered package(s) via --no-verify / OCX_NO_VERIFY");
        }
    }

    async fn auto_verify_rounds(
        &self,
        auto_verify: &AutoVerify,
        resolved: &ocx_oci::PackageRef,
        committed_only: bool,
    ) -> Result<bool, PackageErrorKind> {
        auto_verify
            .refuse_locked_opt_out(resolved.registry())
            .map_err(|refusal| PackageErrorKind::Internal(refusal.into()))?;
        let input = SignerRoundsInput {
            logical: resolved,
            index: self.index(),
            config: &auto_verify.config,
            operator_policies: &auto_verify.operator_policies,
            project_policies: &auto_verify.project_policies,
        };
        let decision = match if committed_only {
            signer_rounds_for_pinned(input).await
        } else {
            signer_rounds(input).await
        } {
            Ok(decision) => decision,
            // Only an unreadable or unparseable committed root yields to the opt-out; any other error, a malformed operator policy included, still refuses.
            Err(
                error @ (crate::Error::InternalFile(..)
                | crate::Error::FileStructure(ocx_store::file_structure::error::Error::MalformedRootDocument {
                    ..
                })),
            ) if auto_verify.user_opted_out => {
                log::debug!(
                    "committed index root unreadable for '{resolved}', skipping verification: {chain}",
                    chain = ocx_util::error::render_chain(&error)
                );
                self.warn_skipped_once(auto_verify);
                return Ok(false);
            }
            Err(error) => return Err(PackageErrorKind::Internal(error)),
        };

        if matches!(decision, RoundsDecision::Uncovered) {
            log::info!(
                "no trust policy covers '{}/{}'; installing '{resolved}' without signature verification",
                resolved.registry(),
                resolved.repository()
            );
            return Ok(false);
        }

        // Covered or refused from here: every exit is a verify or the explicit opt-out, never a silent skip.
        if auto_verify.user_opted_out {
            self.warn_skipped_once(auto_verify);
            return Ok(false);
        }
        let mut rounds = match decision {
            RoundsDecision::Covered(rounds) => rounds,
            RoundsDecision::Refused(refusal) => return Err(PackageErrorKind::Internal(refusal.into())),
            RoundsDecision::Uncovered => return Ok(false),
        };

        let (trust_root, transparency, keyless_skipped) = self.round_trust(auto_verify, resolved, &mut rounds).await?;

        // Publisher round first: its refusal names the publishers, which is the more useful message when both fail.
        if let Some(publishers) = &rounds.publishers {
            self.verify_round(
                auto_verify,
                resolved,
                std::slice::from_ref(publishers),
                trust_root,
                transparency,
            )
            .await
            .map_err(|kind| {
                let kind = publisher_round_error(resolved, std::slice::from_ref(publishers), kind);
                if keyless_skipped {
                    note_keyless_skipped(kind)
                } else {
                    kind
                }
            })?;
        }
        if !rounds.operator.is_empty() {
            self.verify_round(auto_verify, resolved, &rounds.operator, trust_root, transparency)
                .await?;
        }

        log::debug!("auto-verify passed for '{resolved}'");
        Ok(rounds.publishers.is_some())
    }

    /// The trust root and transparency treatment for this package's rounds.
    ///
    /// A keyless candidate needs the full root, resolved at most once per batch. A key candidate needs none: its
    /// log entry is checked only against a root already local, so a key-only install on a fresh home makes no
    /// Sigstore request. When the full root cannot be had and a round also lists a key, the keyless candidates
    /// fail alone and the key candidates still count; a keyless-only round keeps the resolution error.
    async fn round_trust<'a>(
        &self,
        auto_verify: &'a AutoVerify,
        resolved: &ocx_oci::PackageRef,
        rounds: &mut SignerRounds,
    ) -> Result<(&'a TrustRoot, Transparency, bool), PackageErrorKind> {
        let backends = || {
            rounds
                .publishers
                .iter()
                .chain(&rounds.operator)
                .flat_map(|policy| &policy.backends)
        };
        let has_keyless = backends().any(|backend| matches!(backend, ocx_trust::PolicyBackend::Keyless(_)));
        let has_key = backends().any(|backend| matches!(backend, ocx_trust::PolicyBackend::Key(_)));

        let mut keyless_skipped = false;
        if has_keyless {
            match self.auto_verify_trust_root(auto_verify, resolved).await {
                Ok(root) => return Ok((root, Transparency::Verify, false)),
                Err(error) if has_key => {
                    // A keyless candidate on a root that cannot check it aborts the scan, so it is dropped from
                    // the rounds instead; a round left with no key to try keeps the resolution error.
                    log::debug!(
                        "full trust root unavailable, verifying key candidates only: {chain}",
                        chain = ocx_util::error::render_chain(&error)
                    );
                    if !keep_key_backends(rounds) {
                        return Err(error);
                    }
                    keyless_skipped = true;
                }
                Err(error) => return Err(error),
            }
        }

        let signing_state = SigningStatePaths::new(auto_verify.state.root());
        let rekor_cache_key = cache_key_for_rekor(&auto_verify.rekor_url);
        let local = auto_verify
            .local_trust_root
            .get_or_try_init(|| async {
                let local = resolve_local_trust_root(
                    auto_verify.trusted_root_env.as_deref(),
                    auto_verify.sigstore_trust.as_ref(),
                    auto_verify.home_trusted_root.as_deref(),
                    &signing_state,
                    &rekor_cache_key,
                )
                .await?;
                Ok(local.unwrap_or_default())
            })
            .await
            .map_err(|kind| verify_kind(resolved, kind))?;
        Ok((local, Transparency::VerifyIfRootAvailable, keyless_skipped))
    }

    /// The batch's trust root, resolved on first use and memoized on success only, so a failure retries
    /// fail-closed on the next covered package.
    async fn auto_verify_trust_root<'a>(
        &self,
        auto_verify: &'a AutoVerify,
        resolved: &ocx_oci::PackageRef,
    ) -> Result<&'a TrustRoot, PackageErrorKind> {
        // The same set the verify pipeline's SSRF floor reads, so the two guards never disagree on what is exempt.
        let trusted = self.index().trusted_hosts_for(resolved.registry());
        let signing_state = SigningStatePaths::new(auto_verify.state.root());
        auto_verify
            .trust_root
            .get_or_try_init(|| async {
                let root = resolve_trust_root(
                    auto_verify.trusted_root_env.as_deref(),
                    auto_verify.sigstore_trust.as_ref(),
                    auto_verify.home_trusted_root.as_deref(),
                    &signing_state,
                    &cache_key_for_rekor(&auto_verify.rekor_url),
                    auto_verify.offline,
                )
                .await?;
                // No pinned Rekor key: fetch it once into the memoized root for the whole batch.
                if !auto_verify.offline && root.rekor_public_key_pem().is_none() {
                    // Outside the pipeline's SSRF floor, so re-applied here or this endpoint is reached unvalidated
                    // (CWE-918).
                    ocx_oci::endpoint::resolve_sigstore_url(&auto_verify.rekor_url, trusted)
                        .await
                        .map_err(|error| VerifyErrorKind::InvalidEndpointUrl {
                            endpoint: "Rekor endpoint".into(),
                            reason: ocx_oci::endpoint::UrlRejection::from(error),
                        })?;
                    let key = ocx_sign::verify::pipeline::fetch_rekor_public_key_pem(&auto_verify.rekor_url).await?;
                    root.with_rekor_key_pem(&key)
                } else {
                    Ok(root)
                }
            })
            .await
            .map_err(|kind| verify_kind(resolved, kind))
    }

    /// One signer round: first match only, signatures only.
    async fn verify_round(
        &self,
        auto_verify: &AutoVerify,
        resolved: &ocx_oci::PackageRef,
        policies: &[CompiledPolicy],
        trust_root: &TrustRoot,
        transparency: Transparency,
    ) -> Result<(), PackageErrorKind> {
        let options = VerifyOptions {
            policies,
            client: &auto_verify.registry_client,
            trust_root,
            rekor_url: &auto_verify.rekor_url,
            offline: auto_verify.offline,
            state: &auto_verify.state,
            no_cache: false,
            // Signatures only: a subject whose referrers are all attestations fails closed.
            content: VerifyContentMode::Signature,
            // Default discovery order and no unlogged keyless sidecar: an install gate must not widen what counts
            // as signed.
            signature_format: None,
            allow_unlogged_signature: false,
            // First match only: `true` runs full crypto over every candidate on every covered install, unread.
            report_all: false,
            transparency,
        };
        self.verify_one(resolved, None, options)
            .await
            .map(|_| ())
            .map_err(|error| error.kind)
    }
}

/// Strips the keyless backends from every round; `false` (rounds untouched) when a round would be left empty.
fn keep_key_backends(rounds: &mut SignerRounds) -> bool {
    fn key_only(policy: &CompiledPolicy) -> Option<CompiledPolicy> {
        let backends: Vec<_> = policy
            .backends
            .iter()
            .filter(|backend| matches!(backend, ocx_trust::PolicyBackend::Key(_)))
            .cloned()
            .collect();
        (!backends.is_empty()).then(|| CompiledPolicy {
            builder: policy.builder.clone(),
            backends,
        })
    }
    let publishers = rounds.publishers.as_ref().map(key_only);
    let operator: Vec<_> = rounds.operator.iter().map(key_only).collect();
    if matches!(publishers, Some(None)) || operator.iter().any(Option::is_none) {
        return false;
    }
    rounds.publishers = publishers.flatten();
    rounds.operator = operator.into_iter().flatten().collect();
    true
}

/// Wrap a [`VerifyErrorKind`] as a package-manager error, keeping the verify exit code.
fn verify_kind(identifier: &ocx_oci::PackageRef, kind: VerifyErrorKind) -> PackageErrorKind {
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
    use crate::tasks::signer_rounds::RoundsError;
    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use ocx_sign::verify::trust_cache::TrustRootCache;
    use ocx_store::file_structure::FileStructure;

    const REGISTRY: &str = "example.com";
    const REPO: &str = "widget";

    /// The cloud-metadata link-local address: forbidden by the SSRF floor, and
    /// an IP literal, so `lookup_host` answers from the literal itself — the
    /// guarded path opens no socket and needs no DNS.
    const FORBIDDEN_REKOR: &str = "https://169.254.169.254";

    fn covering_policy() -> TrustPolicy {
        TrustPolicy {
            scope: Some(ocx_trust::ScopeSpec::Prefix(format!("{REGISTRY}/{REPO}"))),
            builder: None,
            signers: vec![ocx_trust::SignerSpec::Keyless(ocx_trust::KeylessMatcher {
                identity: Some("you@example.com".into()),
                identity_regexp: None,
                oidc_issuer: Some("https://example.com".into()),
                claims: Default::default(),
            })],
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
        .write_cache(&SigningStatePaths::new(state.root()))
        .await
        .unwrap();

        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(&file_structure),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        let auto_verify = AutoVerify::new(AutoVerifyInput {
            operator_policies: vec![covering_policy()],
            project_policies: Vec::new(),
            config: Config::default(),
            registry_client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new()))),
            rekor_url,
            offline: false,
            state,
            trusted_root_env: None,
            sigstore_trust: None,
            home_trusted_root: None,
            user_opted_out: false,
        });
        let manager = PackageManager::new(file_structure, index, None, REGISTRY).with_auto_verify(Some(auto_verify));

        let target = ocx_oci::PackageRef::new_registry(REPO, REGISTRY).clone_with_tag("1.0");
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

    // ── Q3: the install hot path stays ANY-of first-match ────────────────────

    /// The golden captures, reused from `oci::verify::pipeline`'s own tests: two
    /// genuinely different signatures over one subject, one keyless and one
    /// key-mode, verifiable offline against the committed trust root.
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../test/tests/fixtures/golden/keyless_bundle.json");
    const GOLDEN_KEY_BUNDLE: &str = include_str!("../../../../test/tests/fixtures/golden/key_bundle.json");
    const GOLDEN_KEYLESS_REFERRER: &str =
        include_str!("../../../../test/tests/fixtures/golden/keyless_referrer_manifest.json");
    const GOLDEN_KEY_REFERRER: &str = include_str!("../../../../test/tests/fixtures/golden/key_referrer_manifest.json");
    const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../../test/tests/fixtures/golden/keys/cosign.pub");
    const GOLDEN_TRUSTED_ROOT: &str = include_str!("../../../../test/sigstore/trusted_root.json");
    const GOLDEN_SUBJECT_MANIFEST: &str = concat!(
        r#"{"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json", "#,
        r#""config": {"mediaType": "application/vnd.oci.empty.v1+json", "#,
        r#""digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a", "size": 2}, "#,
        r#""layers": [{"mediaType": "application/octet-stream", "#,
        r#""digest": "sha256:ee88d8a4c22bbe871bcee1c56bcc02377e249363600edcaf096ad7a5a862149f", "size": 18}]}"#,
    );
    const GOLDEN_IDENTITY: &str = "ocx-test@example.com";
    const GOLDEN_ISSUER: &str = "http://dex:5556/dex";

    fn referrer_annotation(manifest_json: &str) -> String {
        let manifest: serde_json::Value = serde_json::from_str(manifest_json).expect("referrer manifest is JSON");
        manifest["annotations"]["dev.sigstore.bundle.predicateType"]
            .as_str()
            .expect("the capture carries the predicateType annotation")
            .to_owned()
    }

    /// An index that resolves the target straight to the golden subject digest,
    /// with no physical rewrite — the smallest thing `resolve_target` accepts.
    #[derive(Clone)]
    struct GoldenSubjectIndex {
        digest: ocx_oci::Digest,
        /// What the committed root declares as `publishers`.
        declared: Option<serde_json::Value>,
        /// Serves a root document and a routing pointer, as a published source does.
        routes: bool,
    }

    #[async_trait::async_trait]
    impl ocx_index::IndexImpl for GoldenSubjectIndex {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &ocx_oci::PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
            // A valid config digest, or `resolve` refuses the flat manifest as `DigestMissing`.
            let mut manifest = ocx_oci::ImageManifest::default();
            manifest.config.digest = self.digest.to_string();
            Ok(Some((self.digest.clone(), ocx_oci::Manifest::Image(manifest))))
        }
        async fn fetch_manifest_digest(
            &self,
            _: &ocx_oci::PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<ocx_oci::Digest>> {
            Ok(Some(self.digest.clone()))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn declared_publishers(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> ocx_index::error::Result<ocx_index::DeclaredPublishers> {
            Ok(self.declared.clone().map_or(
                ocx_index::DeclaredPublishers::Undeclared,
                ocx_index::DeclaredPublishers::Declared,
            ))
        }
        async fn fetch_root_document(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> ocx_index::error::Result<Option<(Vec<u8>, ocx_index::IndexRoot)>> {
            if !self.routes {
                return Ok(None);
            }
            let document = serde_json::json!({
                "repository": format!("oci://{REGISTRY}/{REPO}"),
                "publishers": self.declared,
                "tags": {},
            });
            let root = serde_json::from_value(document.clone()).expect("the root parses");
            Ok(Some((serde_json::to_vec(&document).unwrap(), root)))
        }
        async fn physical_reference(
            &self,
            identifier: &ocx_oci::PackageRef,
        ) -> ocx_index::error::Result<Option<ocx_oci::OciIdentifier>> {
            if !self.routes {
                return Ok(None);
            }
            let pointer = ocx_oci::OciIdentifier::parse_repository_pointer(&format!("oci://{REGISTRY}/{REPO}"))
                .expect("the pointer parses");
            Ok(Some(pointer.at_version_of(identifier)))
        }
        // The chain reads declared publishers only from a published source that serves the registry.
        fn serves_registry(&self, registry: &str) -> bool {
            registry == REGISTRY
        }
        fn source_kind(&self) -> ocx_index::SourceKind {
            ocx_index::SourceKind::Published
        }
        fn box_clone(&self) -> Box<dyn ocx_index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Both golden signatures (keyless, then key) as referrers of the golden subject.
    fn golden_transport_data(subject: &ocx_oci::Digest) -> ocx_oci::client::test_transport::StubTransportData {
        use ocx_oci::client::test_transport::{StubTransportData, referrers_key};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let data = StubTransportData::new();
        // Parsed, never direct-constructed: the constructors are seam-only
        // so tests cannot build one by hand, and this is the same reference `transport_reference`
        // hands the pipeline for this target.
        let image: ocx_oci::native::Reference = format!("{REGISTRY}/{REPO}@{subject}")
            .parse()
            .expect("the golden subject reference parses");
        {
            let mut inner = data.write();
            inner
                .manifests
                .insert(image.to_string(), (subject_bytes.to_vec(), subject.to_string()));
            for (bundle_json, referrer_json) in [
                (GOLDEN_KEYLESS_BUNDLE, GOLDEN_KEYLESS_REFERRER),
                (GOLDEN_KEY_BUNDLE, GOLDEN_KEY_REFERRER),
            ] {
                let blob = bundle_json.as_bytes().to_vec();
                let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);
                let manifest = ocx_oci::referrer::ReferrerManifest::build(
                    ocx_oci::Descriptor {
                        media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                        digest: subject.to_string(),
                        size: subject_bytes.len() as i64,
                        ..ocx_oci::Descriptor::default()
                    },
                    ocx_oci::referrer::media_types::SIGSTORE_BUNDLE_V03,
                    ocx_oci::Descriptor {
                        media_type: ocx_oci::referrer::media_types::SIGSTORE_BUNDLE_V03.to_string(),
                        digest: blob_digest.to_string(),
                        size: blob.len() as i64,
                        ..ocx_oci::Descriptor::default()
                    },
                    Some(std::collections::BTreeMap::from([(
                        "dev.sigstore.bundle.predicateType".to_string(),
                        referrer_annotation(referrer_json),
                    )])),
                );
                let bytes = manifest.to_canonical_json().expect("referrer manifest serializes");
                let digest = ocx_oci::Algorithm::Sha256.hash(&bytes);
                inner.blobs.insert(blob_digest.to_string(), blob);
                inner.manifests.insert(
                    image.clone_with_digest(digest.to_string()).to_string(),
                    (bytes.clone(), digest.to_string()),
                );
                inner
                    .referrers
                    .entry(referrers_key(&image, subject))
                    .or_default()
                    .push(ocx_oci::Descriptor {
                        media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                        digest: digest.to_string(),
                        size: bytes.len() as i64,
                        ..ocx_oci::Descriptor::default()
                    });
            }
        }
        data
    }

    /// What varies between the golden-subject install scenarios.
    struct GoldenScenario {
        declared: Option<serde_json::Value>,
        operator_policies: Vec<TrustPolicy>,
        signatures: Option<&'static str>,
        /// The `signatures` entry came from the system config tier.
        signatures_locked: bool,
        user_opted_out: bool,
        offline: bool,
        /// `OCX_SIGSTORE_TRUSTED_ROOT`; `None` leaves no local trust root at all.
        trusted_root: Option<std::path::PathBuf>,
        /// The source routes the name, so a materializing resolve records a routing pointer.
        routes: bool,
    }

    impl GoldenScenario {
        fn new(declared: Option<serde_json::Value>) -> Self {
            Self {
                declared,
                operator_policies: Vec::new(),
                signatures: None,
                signatures_locked: false,
                user_opted_out: false,
                offline: true,
                trusted_root: None,
                routes: false,
            }
        }
    }

    fn golden_subject() -> (ocx_oci::Digest, ocx_oci::PackageRef) {
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let target = ocx_oci::PackageRef::new_registry(REPO, REGISTRY).clone_with_digest(subject.clone());
        (subject, target)
    }

    fn golden_manager(root: &std::path::Path, data: StubTransportData, scenario: GoldenScenario) -> PackageManager {
        use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
        use ocx_oci::client::test_transport::StubTransport;

        let (subject, _) = golden_subject();
        let file_structure = FileStructure::with_root(root.to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(&file_structure),
            }),
            vec![Index::from_impl(GoldenSubjectIndex {
                digest: subject,
                declared: scenario.declared,
                routes: scenario.routes,
            })],
            ChainMode::Default,
        );
        let config = match scenario.signatures {
            Some(value) => {
                let mut config: Config =
                    toml::from_str(&format!("[registries.\"{REGISTRY}\"]\nsignatures = \"{value}\"\n"))
                        .expect("config parses");
                if scenario.signatures_locked {
                    config
                        .registries
                        .iter_mut()
                        .flatten()
                        .for_each(|entry| entry.1.lock_as_system());
                }
                config
            }
            None => Config::default(),
        };
        let auto_verify = AutoVerify::new(AutoVerifyInput {
            operator_policies: scenario.operator_policies,
            project_policies: Vec::new(),
            config,
            registry_client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(data))),
            rekor_url: Url::parse("http://127.0.0.1:3000").unwrap(),
            offline: scenario.offline,
            state: StateStore::new(root.join("state")),
            trusted_root_env: scenario.trusted_root,
            sigstore_trust: None,
            home_trusted_root: None,
            user_opted_out: scenario.user_opted_out,
        });
        PackageManager::new(file_structure, index, None, REGISTRY).with_auto_verify(Some(auto_verify))
    }

    fn golden_keyless_publisher(identity: &str) -> serde_json::Value {
        serde_json::json!({"kind": "keyless", "identity": identity, "oidc_issuer": GOLDEN_ISSUER})
    }

    fn golden_key_publisher() -> serde_json::Value {
        serde_json::json!({"kind": "key", "key_pem": GOLDEN_PUBLIC_KEY_PEM})
    }

    /// Auto-verify runs on the install hot path and renders no report,
    /// so it must stay ANY-of first-match: exactly one candidate is fetched and
    /// verified, however many the subject carries.
    ///
    /// Observed through the transport, not through a flag: the subject here
    /// carries **two** signatures cosign really wrote, both verifiable under the
    /// one policy below. A `report_all` auto-verify would pull the second
    /// referrer manifest and pay full crypto on it for output nobody reads.
    ///
    /// The control is the same fixture driven through `verify_one` with
    /// `report_all: true`, which does pull it — without that half, a seed that
    /// only ever produced one candidate would satisfy this vacuously.
    #[tokio::test(flavor = "multi_thread")]
    async fn auto_verify_examines_one_candidate_however_many_the_subject_carries() {
        let (subject, target) = golden_subject();

        let root = TempDir::new().unwrap();
        let trusted_root_path = root.path().join("trusted_root.json");
        std::fs::write(&trusted_root_path, GOLDEN_TRUSTED_ROOT).unwrap();

        let policy = TrustPolicy {
            scope: Some(ocx_trust::ScopeSpec::Prefix(format!("{REGISTRY}/{REPO}"))),
            builder: None,
            signers: vec![
                ocx_trust::SignerSpec::Keyless(ocx_trust::KeylessMatcher {
                    identity: Some(GOLDEN_IDENTITY.into()),
                    identity_regexp: None,
                    oidc_issuer: Some(GOLDEN_ISSUER.into()),
                    claims: Default::default(),
                }),
                ocx_trust::SignerSpec::Key(ocx_trust::KeyMatcher {
                    key: None,
                    key_pem: Some(GOLDEN_PUBLIC_KEY_PEM.to_string()),
                }),
            ],
            system_locked: false,
        };

        let seed = || golden_transport_data(&subject);
        // The trust services are never dialled: the committed root pins the Rekor key, which is what makes this
        // test hermetic.
        let build_manager = |data: StubTransportData| {
            golden_manager(
                root.path(),
                data,
                GoldenScenario {
                    operator_policies: vec![policy.clone()],
                    trusted_root: Some(trusted_root_path.clone()),
                    ..GoldenScenario::new(None)
                },
            )
        };

        fn manifest_pulls(data: &StubTransportData) -> usize {
            data.read()
                .calls
                .iter()
                .filter(|call| *call == "pull_manifest_raw")
                .count()
        }

        let hot_path = seed();
        build_manager(hot_path.clone())
            .maybe_auto_verify(&target)
            .await
            .expect("the subject is signed by a covered identity");
        // One subject manifest + exactly one referrer manifest.
        assert_eq!(
            manifest_pulls(&hot_path),
            2,
            "auto-verify must stop at the first candidate that passes, got: {:?}",
            hot_path.read().calls,
        );

        // The control: the same two candidates under `report_all` do get
        // examined, so the count above is a property of the arity and not of a
        // seed that could only ever produce one candidate.
        let reporting = seed();
        let manager = build_manager(reporting.clone());
        let state = StateStore::new(root.path().join("state"));
        let trust_root = ocx_sign::verify::TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes())
            .expect("the committed trust root loads");
        let policies = ocx_trust::resolve_tiered(&[policy], &[], &format!("{REGISTRY}/{REPO}")).unwrap();
        let rekor_url = Url::parse("http://127.0.0.1:3000").unwrap();
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(reporting.clone())));
        let report = manager
            .verify_one(
                &target,
                None,
                VerifyOptions {
                    policies: &policies,
                    client: &client,
                    trust_root: &trust_root,
                    rekor_url: &rekor_url,
                    offline: true,
                    state: &state,
                    no_cache: false,
                    content: VerifyContentMode::Signature,
                    signature_format: None,
                    allow_unlogged_signature: false,
                    report_all: true,
                    transparency: ocx_sign::verify::Transparency::Verify,
                },
            )
            .await
            .expect("both golden signatures verify");
        assert_eq!(report.signatures.len(), 2, "report_all lists both signatures");
        assert_eq!(
            manifest_pulls(&reporting),
            3,
            "report_all must pull both referrer manifests, got: {:?}",
            reporting.read().calls,
        );
    }

    // ── the publisher round's mapping and the opt-out ─────────

    fn publisher_policy() -> CompiledPolicy {
        ocx_trust::parse_publishers(&serde_json::json!([golden_key_publisher()]))
            .compile()
            .expect("a key publisher compiles")
    }

    fn keyless_only_policy() -> CompiledPolicy {
        CompiledPolicy::exact(GOLDEN_IDENTITY.into(), GOLDEN_ISSUER.into())
    }

    fn verify_failure(kind: VerifyErrorKind) -> PackageErrorKind {
        verify_kind(&golden_subject().1, kind)
    }

    /// The verify kind a refused round carried, whichever layer it ended up in.
    fn round_outcome(kind: PackageErrorKind) -> Result<VerifyErrorKind, RoundsError> {
        match kind {
            PackageErrorKind::Internal(crate::Error::Rounds(error)) => Err(error),
            PackageErrorKind::Internal(crate::Error::Verify(error)) => Ok(error.kind),
            other => panic!("expected a verify or rounds error, got {other:?}"),
        }
    }

    /// "nobody I list signed it" is `PublisherMismatch`, carrying the verify kind as its cause.
    #[test]
    fn c013_identity_and_issuer_mismatch_remap_to_publisher_mismatch() {
        let target = golden_subject().1;
        for kind in [VerifyErrorKind::IdentityMismatch, VerifyErrorKind::IssuerMismatch] {
            let expected = format!("{kind:?}");
            let remapped = publisher_round_error(
                &target,
                std::slice::from_ref(&keyless_only_policy()),
                verify_failure(kind),
            );
            match round_outcome(remapped) {
                Err(RoundsError::PublisherMismatch { reason, package, .. }) => {
                    assert_eq!(format!("{:?}", reason.expect("a verify failure")), expected);
                    assert_eq!(package, target.to_string());
                }
                other => panic!("expected PublisherMismatch, got {other:?}"),
            }
        }
    }

    /// Against a key publisher an unverified signature is "no listed key signed it".
    #[test]
    fn c013_signature_invalid_remaps_only_when_a_key_publisher_is_listed() {
        let target = golden_subject().1;
        let remapped = publisher_round_error(
            &target,
            std::slice::from_ref(&publisher_policy()),
            verify_failure(VerifyErrorKind::SignatureInvalid),
        );
        assert!(matches!(
            round_outcome(remapped),
            Err(RoundsError::PublisherMismatch { .. })
        ));

        let kept = publisher_round_error(
            &target,
            std::slice::from_ref(&keyless_only_policy()),
            verify_failure(VerifyErrorKind::SignatureInvalid),
        );
        assert!(matches!(round_outcome(kept), Ok(VerifyErrorKind::SignatureInvalid)));
    }

    /// Every other kind passes through with its own exit code.
    #[test]
    fn c013_other_kinds_pass_through_unchanged() {
        let target = golden_subject().1;
        for kind in [
            VerifyErrorKind::NoSignaturesFound,
            VerifyErrorKind::CertChainInvalid,
            VerifyErrorKind::TransparencyLogUnavailable,
        ] {
            let expected = format!("{kind:?}");
            let passed =
                publisher_round_error(&target, std::slice::from_ref(&publisher_policy()), verify_failure(kind));
            match round_outcome(passed) {
                Ok(kept) => assert_eq!(format!("{kept:?}"), expected),
                Err(error) => panic!("{expected} must pass through, got {error:?}"),
            }
        }
    }

    fn rounds_refusal(error: PackageErrorKind) -> RoundsError {
        match round_outcome(error) {
            Err(error) => error,
            Ok(kind) => panic!("expected a rounds refusal, got verify kind {kind:?}"),
        }
    }

    /// End to end: a publisher list that names nobody who signed the subject refuses 77 `publisher_mismatch`.
    #[tokio::test(flavor = "multi_thread")]
    async fn c013_a_subject_signed_by_nobody_listed_is_refused() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let trusted_root = root.path().join("trusted_root.json");
        std::fs::write(&trusted_root, GOLDEN_TRUSTED_ROOT).unwrap();
        let scenario = GoldenScenario {
            trusted_root: Some(trusted_root),
            ..GoldenScenario::new(Some(serde_json::json!([golden_keyless_publisher(
                "someone-else@example.com"
            )])))
        };
        let error = golden_manager(root.path(), golden_transport_data(&subject), scenario)
            .maybe_auto_verify(&target)
            .await
            .expect_err("nobody listed signed the subject");
        assert!(matches!(rounds_refusal(error), RoundsError::PublisherMismatch { .. }));
    }

    /// End to end: a listed publisher's signature alone passes, with no operator policy at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn c010_a_listed_publisher_alone_covers_the_install() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let trusted_root = root.path().join("trusted_root.json");
        std::fs::write(&trusted_root, GOLDEN_TRUSTED_ROOT).unwrap();
        let scenario = GoldenScenario {
            trusted_root: Some(trusted_root),
            ..GoldenScenario::new(Some(serde_json::json!([golden_keyless_publisher(GOLDEN_IDENTITY)])))
        };
        golden_manager(root.path(), golden_transport_data(&subject), scenario)
            .maybe_auto_verify(&target)
            .await
            .expect("the listed publisher signed the subject");
    }

    /// A publisher AND an operator policy both apply: a passing publisher round does not stand in for the
    /// operator's, whose own mismatch keeps its own slug (`identity_mismatch`, not `publisher_mismatch`).
    #[tokio::test(flavor = "multi_thread")]
    async fn c010_the_operator_round_still_runs_after_the_publisher_round_passes() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let trusted_root = root.path().join("trusted_root.json");
        std::fs::write(&trusted_root, GOLDEN_TRUSTED_ROOT).unwrap();
        let scenario = GoldenScenario {
            operator_policies: vec![covering_policy()],
            trusted_root: Some(trusted_root),
            ..GoldenScenario::new(Some(serde_json::json!([golden_keyless_publisher(GOLDEN_IDENTITY)])))
        };
        let error = golden_manager(root.path(), golden_transport_data(&subject), scenario)
            .maybe_auto_verify(&target)
            .await
            .expect_err("the operator's identity did not sign the subject");
        assert!(
            matches!(round_outcome(error), Ok(VerifyErrorKind::IdentityMismatch)),
            "the operator round reports the verify kind, not the publisher refusal"
        );
    }

    /// A root only a source fetched (the routing read of a digest or lock install) declares nothing for
    /// a request that names a digest; `required` then refuses 81 naming `ocx index update`.
    #[tokio::test(flavor = "multi_thread")]
    async fn r8_a_digest_request_ignores_a_root_only_a_source_fetched() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let declared = || {
            Some(serde_json::json!([golden_keyless_publisher(
                "someone-else@example.com"
            )]))
        };
        let tagged = target.clone_with_tag("1.0");

        let installs = GoldenScenario::new(declared());
        golden_manager(root.path(), golden_transport_data(&subject), installs)
            .maybe_auto_verify_requested(&target, &target)
            .await
            .expect("a digest request installs undeclared");

        let required = GoldenScenario {
            signatures: Some("required"),
            ..GoldenScenario::new(declared())
        };
        let error = golden_manager(root.path(), golden_transport_data(&subject), required)
            .maybe_auto_verify_requested(&target, &target)
            .await
            .expect_err("required refuses an undeclared digest install");
        match rounds_refusal(error) {
            RoundsError::PublishersRequired { root, .. } => assert!(!root.is_empty()),
            other => panic!("expected PublishersRequired, got {other:?}"),
        }

        // A tag request adopts the fetched list, so the same source refuses (control: the digest is what differs).
        let tagged_scenario = GoldenScenario::new(declared());
        golden_manager(root.path(), golden_transport_data(&subject), tagged_scenario)
            .maybe_auto_verify_requested(&target, &tagged)
            .await
            .expect_err("a tag request verifies against the fetched publishers");
    }

    /// `--no-verify` skips a refusal too, with exactly one warning for the batch.
    #[tokio::test(flavor = "multi_thread")]
    async fn c015_opt_out_skips_a_refused_decision_with_one_warning() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let refused = || GoldenScenario {
            signatures: Some("required"),
            ..GoldenScenario::new(None)
        };

        // Without the opt-out the same scenario refuses: the opt-out is what turns 81 into a skip.
        let manager = golden_manager(root.path(), golden_transport_data(&subject), refused());
        let error = manager
            .maybe_auto_verify(&target)
            .await
            .expect_err("`required` without publishers refuses");
        assert!(matches!(rounds_refusal(error), RoundsError::PublishersRequired { .. }));

        let manager = golden_manager(
            root.path(),
            golden_transport_data(&subject),
            GoldenScenario {
                user_opted_out: true,
                ..refused()
            },
        );
        let av = manager.auto_verify().expect("attached").clone();
        assert!(!av.warned.load(Ordering::Relaxed));
        manager.maybe_auto_verify(&target).await.expect("opted out");
        manager.maybe_auto_verify(&target).await.expect("opted out");
        assert!(av.warned.load(Ordering::Relaxed), "the warning latch fired");
    }

    /// A system-locked `required` refuses `--no-verify` with `NoVerifyRefused` on the rounds path, and the
    /// unlocked twin keeps the opt-out (one warning, installs).
    #[tokio::test(flavor = "multi_thread")]
    async fn d13_a_locked_required_refuses_the_opt_out_on_the_rounds_path() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let scenario = |signatures_locked| GoldenScenario {
            signatures: Some("required"),
            signatures_locked,
            user_opted_out: true,
            ..GoldenScenario::new(None)
        };

        let manager = golden_manager(root.path(), golden_transport_data(&subject), scenario(true));
        let error = manager
            .maybe_auto_verify(&target)
            .await
            .expect_err("a locked `required` refuses the opt-out");
        match rounds_refusal(error) {
            RoundsError::NoVerifyRefused { registry, config_path } => {
                assert_eq!(registry, REGISTRY);
                assert!(!config_path.is_empty());
            }
            other => panic!("expected NoVerifyRefused, got {other:?}"),
        }

        let manager = golden_manager(root.path(), golden_transport_data(&subject), scenario(false));
        manager
            .maybe_auto_verify(&target)
            .await
            .expect("unlocked opt-out installs");
        assert!(manager.auto_verify().expect("attached").warned.load(Ordering::Relaxed));
    }

    /// The find fast path refuses too: a locked `required` must not hand out store content on `--no-verify`.
    #[tokio::test(flavor = "multi_thread")]
    async fn d13_a_locked_required_refuses_the_opt_out_on_the_fast_path() {
        let (_, target) = golden_subject();
        let scenario = |signatures_locked| GoldenScenario {
            signatures: Some("required"),
            signatures_locked,
            user_opted_out: true,
            ..declared_scenario()
        };

        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), StubTransportData::new(), scenario(true));
        install_golden_content(&manager, &target);
        let error = find_golden(&manager, &golden_tag()).await.expect_err("locked refuses");
        assert!(matches!(rounds_refusal(error), RoundsError::NoVerifyRefused { .. }));

        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), StubTransportData::new(), scenario(false));
        install_golden_content(&manager, &target);
        find_golden(&manager, &golden_tag())
            .await
            .expect("unlocked opt-out keeps the fast path");
    }

    /// A committed root that cannot be read (a directory stands in for the file: the read fails with an I/O
    /// error other than NotFound, and unlike `chmod 000` that holds for root) or does not parse yields to an
    /// unlocked opt-out, and still errors without one.
    #[tokio::test(flavor = "multi_thread")]
    async fn d13_b_an_unreadable_or_malformed_committed_root_yields_to_the_opt_out_only() {
        let (_, target) = golden_subject();
        for unreadable in [true, false] {
            for user_opted_out in [true, false] {
                let root = TempDir::new().unwrap();
                let manager = golden_manager(
                    root.path(),
                    StubTransportData::new(),
                    GoldenScenario {
                        user_opted_out,
                        ..GoldenScenario::new(None)
                    },
                );
                let path =
                    ocx_index::IndexStore::machine_local(manager.file_structure()).root_document_path(REGISTRY, REPO);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                if unreadable {
                    std::fs::create_dir(&path).unwrap();
                } else {
                    std::fs::write(&path, b"{ not json").unwrap();
                }
                let outcome = manager.maybe_auto_verify_requested(&target, &target).await;
                if user_opted_out {
                    assert!(
                        !outcome.expect("the opt-out proceeds past the unreadable root"),
                        "no publisher round ran"
                    );
                    assert!(manager.auto_verify().expect("attached").warned.load(Ordering::Relaxed));
                } else {
                    outcome.expect_err("without the opt-out the root error stands");
                }
            }
        }
    }

    /// An uncovered package under the opt-out stays silent (today's behaviour).
    #[tokio::test(flavor = "multi_thread")]
    async fn c015_opt_out_of_an_uncovered_package_does_not_warn() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let manager = golden_manager(
            root.path(),
            golden_transport_data(&subject),
            GoldenScenario {
                user_opted_out: true,
                ..GoldenScenario::new(None)
            },
        );
        manager.maybe_auto_verify(&target).await.expect("uncovered");
        assert!(!manager.auto_verify().expect("attached").warned.load(Ordering::Relaxed));
    }

    /// A covered package (publisher declared) under the opt-out is skipped without reading a signature.
    #[tokio::test(flavor = "multi_thread")]
    async fn c015_opt_out_of_a_declared_package_reads_no_signature() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let data = golden_transport_data(&subject);
        let manager = golden_manager(
            root.path(),
            data.clone(),
            GoldenScenario {
                user_opted_out: true,
                ..GoldenScenario::new(Some(serde_json::json!([golden_keyless_publisher(
                    "nobody@example.com"
                )])))
            },
        );
        manager.maybe_auto_verify(&target).await.expect("opted out");
        assert!(
            data.read().calls.is_empty(),
            "no transport call: {:?}",
            data.read().calls
        );
        assert!(manager.auto_verify().expect("attached").warned.load(Ordering::Relaxed));
    }

    // ── the origin gate ───────────────────────────────────────────────

    /// Places the golden subject's content in the store (no origin marker) and returns its directory.
    fn install_golden_content(
        manager: &PackageManager,
        target: &ocx_oci::PackageRef,
    ) -> ocx_store::file_structure::PackageDir {
        let pinned = ocx_oci::PinnedPackageRef::try_from(target.clone()).expect("the golden target is pinned");
        let dir = ocx_store::file_structure::PackageDir {
            dir: manager.file_structure().packages.path(&pinned),
        };
        std::fs::create_dir_all(dir.content()).unwrap();
        std::fs::write(dir.metadata(), r#"{"type":"bundle","version":1}"#).unwrap();
        std::fs::write(dir.resolve(), r#"{"dependencies":[]}"#).unwrap();
        dir
    }

    /// The request a user types: a tag, which reads the effective publisher list.
    fn golden_tag() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(REPO, REGISTRY).clone_with_tag("1.0")
    }

    /// Commits a root declaring `publishers` under `root`, as `ocx index update` leaves it.
    async fn commit_golden_root(root: &std::path::Path, publishers: serde_json::Value) {
        let store = ocx_index::IndexStore::machine_local(&FileStructure::with_root(root.to_path_buf()));
        let document = serde_json::json!({
            "repository": format!("oci://{REGISTRY}/{REPO}"),
            "publishers": publishers,
            "tags": {},
        });
        store
            .write_root_document(REGISTRY, REPO, &serde_json::to_vec(&document).unwrap())
            .await
            .unwrap();
    }

    fn declared_scenario() -> GoldenScenario {
        GoldenScenario::new(Some(serde_json::json!([golden_key_publisher()])))
    }

    async fn find_golden(manager: &PackageManager, request: &ocx_oci::PackageRef) -> Result<(), PackageErrorKind> {
        manager.find(request, ocx_oci::Platform::any()).await.map(drop)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c011_declared_content_without_a_verified_marker_is_not_installed() {
        let (_, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), StubTransportData::new(), declared_scenario());
        install_golden_content(&manager, &target);

        let error = find_golden(&manager, &golden_tag())
            .await
            .expect_err("unverified content of a declared package must fall through to the pull");
        assert!(matches!(error, PackageErrorKind::NotFound), "got {error:?}");
    }

    /// The ordinary origin marker is written after any successful pull, `Uncovered` and `--no-verify` included, so
    /// it must not stand in for a publisher check.
    #[tokio::test(flavor = "multi_thread")]
    async fn c011_an_origin_marker_alone_does_not_open_the_gate() {
        let (_, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), StubTransportData::new(), declared_scenario());
        let dir = install_golden_content(&manager, &target);
        ocx_store::file_structure::record_origin(&dir, &target).await.unwrap();

        let error = find_golden(&manager, &golden_tag())
            .await
            .expect_err("an origin proves where content came from, not that its publisher was checked");
        assert!(matches!(error, PackageErrorKind::NotFound), "got {error:?}");

        // Control: the verified marker is what opens it.
        ocx_store::file_structure::record_verified(&dir, &target).await.unwrap();
        find_golden(&manager, &golden_tag())
            .await
            .expect("a verified marker for this repository opens the gate");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c011_a_marker_for_another_repository_does_not_open_the_gate() {
        let (_, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), StubTransportData::new(), declared_scenario());
        let dir = install_golden_content(&manager, &target);
        let elsewhere =
            ocx_oci::PackageRef::new_registry("other", REGISTRY).clone_with_digest(target.digest().unwrap());
        ocx_store::file_structure::record_verified(&dir, &elsewhere)
            .await
            .unwrap();

        let error = find_golden(&manager, &golden_tag())
            .await
            .expect_err("a marker for a different repository proves nothing here");
        assert!(matches!(error, PackageErrorKind::NotFound), "got {error:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c011_undeclared_content_keeps_the_store_hit_fast_path() {
        let (_, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), StubTransportData::new(), GoldenScenario::new(None));
        install_golden_content(&manager, &target);

        find_golden(&manager, &golden_tag())
            .await
            .expect("an undeclared package needs no marker");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c011_opt_out_and_off_keep_the_fast_path_for_declared_content() {
        let (_, target) = golden_subject();
        for scenario in [
            GoldenScenario {
                user_opted_out: true,
                ..declared_scenario()
            },
            GoldenScenario {
                signatures: Some("off"),
                ..declared_scenario()
            },
        ] {
            let root = TempDir::new().unwrap();
            let manager = golden_manager(root.path(), StubTransportData::new(), scenario);
            install_golden_content(&manager, &target);
            find_golden(&manager, &golden_tag())
                .await
                .expect("opting out or `off` skips the origin gate");
        }
    }

    /// Marks the golden content's install as complete, as a finished pull does.
    async fn mark_golden_installed(manager: &PackageManager, target: &ocx_oci::PackageRef) {
        // The stub index serves a manifest that does not hash to its digest; a staged blob skips that fetch.
        let (subject, _) = golden_subject();
        manager
            .file_structure()
            .blobs
            .write_blob(REGISTRY, &subject, GOLDEN_SUBJECT_MANIFEST.as_bytes())
            .await
            .unwrap();
        let pinned = ocx_oci::PinnedPackageRef::try_from(target.clone()).expect("pinned");
        let status = manager.file_structure().packages.install_status(&pinned);
        std::fs::write(
            status,
            serde_json::to_vec(&ocx_package::install_status::InstallStatus::new().ok()).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c011_verified_content_already_in_the_store_takes_the_fast_path_with_no_verify_call() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let data = golden_transport_data(&subject);
        let manager = golden_manager(root.path(), data.clone(), declared_scenario());
        let dir = install_golden_content(&manager, &target);
        mark_golden_installed(&manager, &target).await;
        assert!(matches!(
            find_golden(&manager, &golden_tag()).await,
            Err(PackageErrorKind::NotFound)
        ));

        manager
            .pull(&golden_tag(), ocx_oci::Platform::any())
            .await
            .expect("the pull verifies and finds the content present");
        let verify_calls = data.read().calls.len();
        assert!(verify_calls > 0, "the leader verified");
        assert!(ocx_store::file_structure::is_verified(&dir, &target).await);

        find_golden(&manager, &golden_tag())
            .await
            .expect("the verified marker opens the gate");
        assert_eq!(data.read().calls.len(), verify_calls, "the fast path verifies nothing");
    }

    /// Content installed while the package was undeclared is verified before it is served under the declaration.
    #[tokio::test(flavor = "multi_thread")]
    async fn c011_content_installed_undeclared_is_verified_once_the_package_is_declared() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let data = golden_transport_data(&subject);
        let undeclared = golden_manager(root.path(), data.clone(), GoldenScenario::new(None));
        let dir = install_golden_content(&undeclared, &target);
        mark_golden_installed(&undeclared, &target).await;
        undeclared
            .pull(&golden_tag(), ocx_oci::Platform::any())
            .await
            .expect("an undeclared install passes with no check");
        assert!(!ocx_store::file_structure::is_verified(&dir, &target).await);
        // What a genuine fetch of the same digest also leaves behind.
        ocx_store::file_structure::record_origin(&dir, &target).await.unwrap();

        let declared = golden_manager(root.path(), data.clone(), declared_scenario());
        let error = find_golden(&declared, &golden_tag())
            .await
            .expect_err("the declaration appeared after the install");
        assert!(matches!(error, PackageErrorKind::NotFound), "got {error:?}");

        declared
            .pull(&golden_tag(), ocx_oci::Platform::any())
            .await
            .expect("the fall-through verifies the listed publisher");
        find_golden(&declared, &golden_tag()).await.expect("verified now");
    }

    /// `--no-verify` skips the publisher round, so the install it leaves is not a verified one.
    #[tokio::test(flavor = "multi_thread")]
    async fn c011_a_no_verify_install_is_still_verified_by_the_next_plain_find() {
        let (_, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let opted_out = golden_manager(
            root.path(),
            StubTransportData::new(),
            GoldenScenario {
                user_opted_out: true,
                ..declared_scenario()
            },
        );
        let dir = install_golden_content(&opted_out, &target);
        mark_golden_installed(&opted_out, &target).await;
        opted_out
            .pull(&golden_tag(), ocx_oci::Platform::any())
            .await
            .expect("opted out");
        find_golden(&opted_out, &golden_tag())
            .await
            .expect("the opt-out opens the gate for its own invocation");
        assert!(!ocx_store::file_structure::is_verified(&dir, &target).await);

        let plain = golden_manager(root.path(), StubTransportData::new(), declared_scenario());
        let error = find_golden(&plain, &golden_tag())
            .await
            .expect_err("a later invocation without the opt-out verifies first");
        assert!(matches!(error, PackageErrorKind::NotFound), "got {error:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c011_failed_verification_records_no_marker() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();

        // Positive control: the same flow with the signatures present records the marker.
        let good = golden_manager(root.path(), golden_transport_data(&subject), declared_scenario());
        let dir = install_golden_content(&good, &target);
        mark_golden_installed(&good, &target).await;
        good.pull(&golden_tag(), ocx_oci::Platform::any())
            .await
            .expect("the control verifies");
        assert!(ocx_store::file_structure::is_verified(&dir, &target).await);
        std::fs::remove_dir_all(dir.refs_verified_dir()).unwrap();

        // No signatures in the transport: the declared publisher cannot verify.
        let bad = golden_manager(root.path(), StubTransportData::new(), declared_scenario());
        let error = bad
            .pull(&golden_tag(), ocx_oci::Platform::any())
            .await
            .expect_err("unverifiable content is refused");
        // A pull reports the leader's failure wrapped for its waiters; the cause is still the missing signature.
        assert!(format!("{error:?}").contains("NoSignaturesFound"), "got {error:?}");
        assert!(!ocx_store::file_structure::is_verified(&dir, &target).await);
    }

    /// A digest request reads the committed root alone, as the install-side rounds do: a root a source
    /// fetched declares nothing for it, a committed one still does.
    #[tokio::test(flavor = "multi_thread")]
    async fn r8_the_gate_reads_the_committed_root_for_a_digest_request() {
        let (_, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), StubTransportData::new(), declared_scenario());
        install_golden_content(&manager, &target);

        find_golden(&manager, &target)
            .await
            .expect("only a source declares it, so a digest request stays on the fast path");

        let committed = TempDir::new().unwrap();
        commit_golden_root(committed.path(), serde_json::json!([golden_key_publisher()])).await;
        let manager = golden_manager(committed.path(), StubTransportData::new(), GoldenScenario::new(None));
        install_golden_content(&manager, &target);
        let error = find_golden(&manager, &target)
            .await
            .expect_err("a committed declaration gates a digest request");
        assert!(matches!(error, PackageErrorKind::NotFound), "got {error:?}");
    }

    /// `ocx exec` over an `ocx.lock` pin (tag and digest) in a fresh home resolves the pin in `find`, which
    /// records a routing pointer before the install hook reads the committed root. The pointer must not carry the
    /// source's `publishers`, or the install is held to a declaration the user never adopted: exit 79
    /// `no signatures found` where an undeclared install, or 81 under `required`, is expected.
    #[tokio::test(flavor = "multi_thread")]
    async fn s016_a_lock_pin_resolved_in_a_fresh_home_adopts_no_publishers() {
        let (subject, target) = golden_subject();
        let pin = golden_tag().clone_with_digest(subject.clone());
        for signatures in [None, Some("required")] {
            let root = TempDir::new().unwrap();
            let manager = golden_manager(
                root.path(),
                golden_transport_data(&subject),
                GoldenScenario {
                    routes: true,
                    signatures,
                    ..declared_scenario()
                },
            );
            let miss = find_golden(&manager, &pin).await.expect_err("nothing is installed");
            assert!(matches!(miss, PackageErrorKind::NotInstalled(_)), "got {miss:?}");

            // Non-vacuity: the resolve did commit a pointer; it just declares nothing.
            let store = ocx_index::IndexStore::machine_local(manager.file_structure());
            let committed = store
                .read_root_document_bytes(REGISTRY, REPO)
                .await
                .unwrap()
                .expect("the resolve recorded the routing pointer");
            let committed: serde_json::Value = serde_json::from_slice(&committed).unwrap();
            assert!(committed.get("publishers").is_none(), "{committed}");

            let outcome = manager.maybe_auto_verify_requested(&target, &pin).await;
            match signatures {
                None => assert!(!outcome.expect("an undeclared pin installs")),
                Some(_) => match rounds_refusal(outcome.expect_err("required refuses an undeclared pin")) {
                    RoundsError::PublishersRequired { .. } => {}
                    other => panic!("expected PublishersRequired, got {other:?}"),
                },
            }
        }
    }

    /// Every `find` consults the gate, so the declaration is read once per repository per batch, not per find.
    #[tokio::test(flavor = "multi_thread")]
    async fn c011_the_declaration_is_read_once_per_batch() {
        let (_, target) = golden_subject();
        let root = TempDir::new().unwrap();
        commit_golden_root(root.path(), serde_json::json!([golden_key_publisher()])).await;
        let manager = golden_manager(root.path(), StubTransportData::new(), GoldenScenario::new(None));
        install_golden_content(&manager, &target);
        assert!(matches!(
            find_golden(&manager, &target).await,
            Err(PackageErrorKind::NotFound)
        ));

        let store = ocx_index::IndexStore::machine_local(manager.file_structure());
        std::fs::remove_file(store.root_document_path(REGISTRY, REPO)).unwrap();
        assert!(
            matches!(find_golden(&manager, &target).await, Err(PackageErrorKind::NotFound)),
            "the batch's memo answers; the root is not read again"
        );

        let fresh = golden_manager(root.path(), StubTransportData::new(), GoldenScenario::new(None));
        find_golden(&fresh, &target)
            .await
            .expect("control: a new batch reads the (now absent) root and finds nothing declared");
    }

    /// `--no-pull` serves digest-pinned roots through `find_plain`, which has no gate of its own: the composer's
    /// local probe must close it for a declared package whose content carries no verified marker.
    #[tokio::test(flavor = "multi_thread")]
    async fn c011_the_no_pull_local_probe_honours_the_gate() {
        let (_, target) = golden_subject();
        let platform = ocx_oci::Platform::any();
        let root = TempDir::new().unwrap();
        commit_golden_root(root.path(), serde_json::json!([golden_key_publisher()])).await;
        let manager = golden_manager(root.path(), StubTransportData::new(), GoldenScenario::new(None));
        let dir = install_golden_content(&manager, &target);

        let error = manager
            .local_root(&target, &platform)
            .await
            .expect_err("content no publisher check covered must not be served");
        assert!(matches!(error, PackageErrorKind::NotFound), "got {error:?}");

        // Control: the same probe serves the content once a publisher round has passed for it.
        ocx_store::file_structure::record_verified(&dir, &target).await.unwrap();
        manager
            .local_root(&target, &platform)
            .await
            .expect("verified content is served");
    }

    /// The counter-case of the gate's alignment: a digest install in a home with a committed root still verifies.
    #[tokio::test(flavor = "multi_thread")]
    async fn r8_a_digest_install_with_a_committed_root_still_verifies_and_refuses_a_wrong_signer() {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        commit_golden_root(
            root.path(),
            serde_json::json!([golden_keyless_publisher("someone-else@example.com")]),
        )
        .await;
        let manager = golden_manager(
            root.path(),
            golden_transport_data(&subject),
            GoldenScenario {
                trusted_root: Some({
                    let path = root.path().join("trusted_root.json");
                    std::fs::write(&path, GOLDEN_TRUSTED_ROOT).unwrap();
                    path
                }),
                ..GoldenScenario::new(None)
            },
        );
        let error = manager
            .maybe_auto_verify_requested(&target, &target)
            .await
            .expect_err("the committed publisher did not sign the subject");
        assert!(matches!(rounds_refusal(error), RoundsError::PublisherMismatch { .. }));
    }

    // ── a key candidate needs no Sigstore trust root ──────────────────

    async fn verify_golden_with(scenario: GoldenScenario) -> Result<(), PackageErrorKind> {
        let (subject, target) = golden_subject();
        let root = TempDir::new().unwrap();
        let manager = golden_manager(root.path(), golden_transport_data(&subject), scenario);
        manager.maybe_auto_verify(&target).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c012_a_key_publisher_verifies_with_no_trust_root_at_all() {
        // Offline, no override, no cache, no home root: the full resolve would refuse with
        // `OfflineNoPinnedRekorKey`, which a key candidate must not need.
        verify_golden_with(GoldenScenario::new(Some(serde_json::json!([golden_key_publisher()]))))
            .await
            .expect("a key-signed package verifies without a Sigstore trust root");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c012_a_key_publisher_still_checks_its_log_entry_against_a_local_root() {
        let root = TempDir::new().unwrap();
        let trusted_root_path = root.path().join("trusted_root.json");
        std::fs::write(&trusted_root_path, GOLDEN_TRUSTED_ROOT).unwrap();
        verify_golden_with(GoldenScenario {
            trusted_root: Some(trusted_root_path),
            ..GoldenScenario::new(Some(serde_json::json!([golden_key_publisher()])))
        })
        .await
        .expect("a key-signed package verifies against a local root");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c012_a_keyless_publisher_alone_still_needs_the_full_trust_root() {
        let error = verify_golden_with(GoldenScenario::new(Some(serde_json::json!([
            golden_keyless_publisher(GOLDEN_IDENTITY)
        ]))))
        .await
        .expect_err("a keyless candidate cannot verify offline with no trust root");
        match error {
            PackageErrorKind::Internal(crate::Error::Verify(verify_error)) => {
                assert!(
                    matches!(verify_error.kind, VerifyErrorKind::OfflineNoPinnedRekorKey),
                    "got {:?}",
                    verify_error.kind
                );
            }
            other => panic!("expected Verify(OfflineNoPinnedRekorKey), got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn c012_a_listed_key_carries_the_install_when_the_keyless_trust_root_is_unavailable() {
        verify_golden_with(GoldenScenario::new(Some(serde_json::json!([
            golden_keyless_publisher(GOLDEN_IDENTITY),
            golden_key_publisher()
        ]))))
        .await
        .expect("the key candidate passes while the keyless one fails alone");
    }

    /// A mismatch raised after the keyless publishers were dropped says so; one raised with the full root does not.
    #[tokio::test(flavor = "multi_thread")]
    async fn c012_a_mismatch_after_keyless_publishers_were_skipped_carries_the_trust_root_hint() {
        const UNRELATED_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
            MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEQY+VFWJLFWJpQKBrAqQeDfT1OIou\n\
            YbF3RXwIEbTtElkojhk7HU3uksfEwpgDXqsMtlzxmAXOfXjQUS9nGp8RwA==\n\
            -----END PUBLIC KEY-----\n";
        let error = verify_golden_with(GoldenScenario::new(Some(serde_json::json!([
            golden_keyless_publisher(GOLDEN_IDENTITY),
            {"kind": "key", "key_pem": UNRELATED_KEY_PEM}
        ]))))
        .await
        .expect_err("the only candidate left is a key that did not sign");
        let message = rounds_refusal(error).to_string();
        assert!(message.contains("keyless publishers were skipped"), "{message}");
    }
}
