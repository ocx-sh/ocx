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
    /// so verification narrows into nothing (`platform: None`) — the leaf is
    /// already a flat manifest and the selection has already happened. This is
    /// the same target the pre-C-010 `Platform::any()` argument produced: `any`
    /// was how "do not narrow" had to be spelled while the parameter was
    /// mandatory, and it worked only because a flat manifest advertises `any()`
    /// back. `None` says it directly, and keeps saying it if the leaf ever
    /// stops advertising `any()`.
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
            // No flag reaches the install hot path, so discovery keeps D9's
            // default: prefer a bundle, fall back to a sidecar only when the
            // bundle shape is absent — and never onto a keyless sidecar with no
            // transparency-log evidence, which the same absence of a flag keeps
            // refused. An install-time gate is the last place to widen what
            // counts as signed.
            signature_format: None,
            allow_unlogged_signature: false,
            // Q3. This hook renders no report, so it stays ANY-of first-match
            // and pays crypto for exactly one candidate. `true` here would run
            // full verification over every candidate the caps allow on every
            // install of every policy-covered package, for output nobody reads.
            report_all: false,
        };
        self.verify_one(resolved, None, options)
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
            signers: vec![crate::trust::SignerSpec::Keyless(crate::trust::KeylessMatcher {
                identity: Some("you@example.com".into()),
                identity_regexp: None,
                oidc_issuer: Some("https://example.com".into()),
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

    // ── Q3: the install hot path stays ANY-of first-match ────────────────────

    /// The golden captures, reused from `oci::verify::pipeline`'s own tests: two
    /// genuinely different signatures over one subject, one keyless and one
    /// key-mode, verifiable offline against the committed trust root.
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/keyless_bundle.json");
    const GOLDEN_KEY_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/key_bundle.json");
    const GOLDEN_KEYLESS_REFERRER: &str =
        include_str!("../../../../../test/tests/fixtures/golden/keyless_referrer_manifest.json");
    const GOLDEN_KEY_REFERRER: &str =
        include_str!("../../../../../test/tests/fixtures/golden/key_referrer_manifest.json");
    const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../../../test/tests/fixtures/golden/keys/cosign.pub");
    const GOLDEN_TRUSTED_ROOT: &str = include_str!("../../../../../test/sigstore/trusted_root.json");
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
        digest: oci::Digest,
    }

    #[async_trait::async_trait]
    impl crate::oci::index::IndexImpl for GoldenSubjectIndex {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &oci::Identifier,
            _: crate::oci::index::IndexOperation,
        ) -> crate::Result<Option<(oci::Digest, oci::Manifest)>> {
            Ok(Some((
                self.digest.clone(),
                oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )))
        }
        async fn fetch_manifest_digest(
            &self,
            _: &oci::Identifier,
            _: crate::oci::index::IndexOperation,
        ) -> crate::Result<Option<oci::Digest>> {
            Ok(Some(self.digest.clone()))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn crate::oci::index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// **Q3.** Auto-verify runs on the install hot path and renders no report,
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
        use crate::oci::client::test_transport::{StubTransport, StubTransportData, referrers_key};
        use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject = crate::oci::Algorithm::Sha256.hash(subject_bytes);
        let target = oci::Identifier::new_registry(REPO, REGISTRY).clone_with_digest(subject.clone());

        let seed = || {
            let data = StubTransportData::new();
            // Parsed, never direct-constructed: the constructors are seam-only
            // (T-arch-G1), and this is the same reference `transport_reference`
            // hands the pipeline for this target.
            let image: oci::native::Reference = format!("{REGISTRY}/{REPO}@{subject}")
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
                    let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);
                    let manifest = crate::oci::referrer::ReferrerManifest::build(
                        crate::oci::Descriptor {
                            media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                            digest: subject.to_string(),
                            size: subject_bytes.len() as i64,
                            ..crate::oci::Descriptor::default()
                        },
                        crate::oci::referrer::media_types::SIGSTORE_BUNDLE_V03,
                        crate::oci::Descriptor {
                            media_type: crate::oci::referrer::media_types::SIGSTORE_BUNDLE_V03.to_string(),
                            digest: blob_digest.to_string(),
                            size: blob.len() as i64,
                            ..crate::oci::Descriptor::default()
                        },
                        Some(std::collections::BTreeMap::from([(
                            "dev.sigstore.bundle.predicateType".to_string(),
                            referrer_annotation(referrer_json),
                        )])),
                    );
                    let bytes = manifest.to_canonical_json().expect("referrer manifest serializes");
                    let digest = crate::oci::Algorithm::Sha256.hash(&bytes);
                    inner.blobs.insert(blob_digest.to_string(), blob);
                    inner.manifests.insert(
                        image.clone_with_digest(digest.to_string()).to_string(),
                        (bytes.clone(), digest.to_string()),
                    );
                    inner
                        .referrers
                        .entry(referrers_key(&image, &subject))
                        .or_default()
                        .push(crate::oci::Descriptor {
                            media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                            digest: digest.to_string(),
                            size: bytes.len() as i64,
                            ..crate::oci::Descriptor::default()
                        });
                }
            }
            data
        };

        let root = TempDir::new().unwrap();
        let trusted_root_path = root.path().join("trusted_root.json");
        std::fs::write(&trusted_root_path, GOLDEN_TRUSTED_ROOT).unwrap();

        let policy = TrustPolicy {
            scope: Some(crate::trust::ScopeSpec::Prefix(format!("{REGISTRY}/{REPO}"))),
            builder: None,
            signers: vec![
                crate::trust::SignerSpec::Keyless(crate::trust::KeylessMatcher {
                    identity: Some(GOLDEN_IDENTITY.into()),
                    identity_regexp: None,
                    oidc_issuer: Some(GOLDEN_ISSUER.into()),
                }),
                crate::trust::SignerSpec::Key(crate::trust::KeyMatcher {
                    key: None,
                    key_pem: Some(GOLDEN_PUBLIC_KEY_PEM.to_string()),
                }),
            ],
            system_locked: false,
        };

        let build_manager = |data: StubTransportData| {
            let file_structure = FileStructure::with_root(root.path().to_path_buf());
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: file_structure.index.clone(),
                }),
                vec![Index::from_impl(GoldenSubjectIndex {
                    digest: subject.clone(),
                })],
                ChainMode::Default,
            );
            let auto_verify = AutoVerify::new(AutoVerifyInput {
                operator_policies: vec![policy.clone()],
                project_policies: Vec::new(),
                registry_client: oci::Client::with_transport(Box::new(StubTransport::new(data))),
                rekor_url: Url::parse("http://127.0.0.1:3000").unwrap(),
                // The trust services are never dialled: the committed root pins
                // the Rekor key, which is what makes this test hermetic.
                offline: true,
                state: StateStore::new(root.path().join("state")),
                trusted_root_env: Some(trusted_root_path.clone()),
                sigstore_trust: None,
                home_trusted_root: None,
                user_opted_out: false,
            });
            PackageManager::new(file_structure, index, None, REGISTRY).with_auto_verify(Some(auto_verify))
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
        let trust_root = crate::oci::verify::TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes())
            .expect("the committed trust root loads");
        let policies = crate::trust::resolve_tiered(&[policy], &[], &format!("{REGISTRY}/{REPO}")).unwrap();
        let rekor_url = Url::parse("http://127.0.0.1:3000").unwrap();
        let client = oci::Client::with_transport(Box::new(StubTransport::new(reporting.clone())));
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
}
