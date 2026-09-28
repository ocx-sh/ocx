// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Attach in-toto attestations to target manifests, mirroring [`sign_one`](super::sign).

use url::Url;
use zeroize::Zeroizing;

use crate::error::{PackageError, PackageErrorKind};
use ocx_sign::attest::pipeline::{AttestContext, AttestMode, AttestPipeline, AttestResult};
use ocx_sign::attest::predicate::PredicateType;
use ocx_sign::sign::state::SigningStatePaths;
use ocx_sign::sign::{DispatchingTokenProvider, SignError, SignErrorKind};

use super::super::PackageManager;

/// Options forwarded from the CLI to [`PackageManager::attest_one`]: [`SignOptions`](super::sign::SignOptions)
/// plus the predicate and the `offline` policy flag.
#[derive(Clone)]
pub struct AttestOptions {
    /// Fulcio CA endpoint (validated by the CLI). Default: `https://fulcio.sigstore.dev`.
    pub fulcio_url: Url,
    /// Rekor transparency log endpoint (validated by the CLI). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: Url,
    /// OIDC override token (file / stdin / env, resolved by the CLI layer).
    pub identity_token: Option<Zeroizing<String>>,
    /// The requested `--type`; its resolved URI is what gets written.
    pub predicate_type: PredicateType,
    /// Raw file bytes, spliced verbatim; a parsed `Value` would normalize whitespace and number spelling.
    pub predicate: Vec<u8>,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// When true, suppress the browser OAuth fallback (CI / headless).
    pub no_tty: bool,
    /// Offline-refusal policy, checked before token resolution.
    pub offline: bool,
    /// Selects key mode. `None` is keyless — see [`SignOptions::key`](super::sign::SignOptions::key).
    pub key: Option<ocx_trust::key_ref::KeyRef>,
    /// Whether a transparency-log entry is uploaded — see
    /// [`SignOptions::rekor_upload`](super::sign::SignOptions::rekor_upload).
    pub rekor_upload: bool,
    /// Which cosign wire shape(s) to publish the attestation in — see
    /// [`AttestContext::format`](ocx_sign::attest::pipeline::AttestContext::format).
    pub format: ocx_sign::sign::SignatureFormat,
}

/// Success payload returned by [`PackageManager::attest_one`].
#[derive(Debug)]
pub struct AttestReport {
    /// Raw pipeline result (subject digest, resolved predicate type, referrer descriptor).
    pub result: AttestResult,
}

impl PackageManager {
    /// Attach an in-toto attestation to what `package` resolves to, publishing a DSSE-enveloped Sigstore bundle
    /// v0.3 referrer manifest; `Some(platform)` narrows into an index as [`sign_one`](Self::sign_one) does.
    ///
    /// # Errors
    ///
    /// [`PackageError`] tagged with `package` on any failure; the exit code comes from
    /// [`ocx_sign::sign::SignErrorKind`].
    pub async fn attest_one(
        &self,
        package: &ocx_oci::PackageRef,
        platform: Option<&ocx_oci::Platform>,
        opts: AttestOptions,
        resolved: Option<&(ocx_oci::Digest, ocx_oci::Manifest)>,
    ) -> Result<AttestReport, PackageError> {
        // Refused here too, or `require_client`'s `OfflineMode` (81) shadows the 77 policy code scripts branch on.
        if opts.offline {
            return Err(map_attest_error(
                package.clone(),
                SignError::new(package.clone(), SignErrorKind::OfflineAttestRefused),
            ));
        }

        // `RawValue`, not `Value`: validates without normalizing, so the signed bytes are the file's own.
        let predicate: Box<serde_json::value::RawValue> = serde_json::from_slice(&opts.predicate).map_err(|_| {
            // Parse error discarded: it quotes user-file bytes that would reach the terminal unsanitized.
            map_attest_error(
                package.clone(),
                SignError::new(package.clone(), SignErrorKind::PredicateNotJson),
            )
        })?;

        let client = self
            .require_client()
            .map_err(|e| PackageError::new(package.clone(), PackageErrorKind::Internal(e)))?;

        let signer = super::sign::build_signer(opts.key.as_ref(), opts.rekor_upload, &opts.rekor_url)
            .map_err(|kind| map_attest_error(package.clone(), SignError::new(package.clone(), kind)))?;
        let trusted_hosts = self.index().trusted_hosts_for(package.registry()).to_vec();
        let token_provider = DispatchingTokenProvider::new(opts.identity_token, opts.no_tty, trusted_hosts);
        // Sign iff signing material is visible; `--key` counts, or naming a key degrades to unsigned without OIDC.
        // A failed redemption stays a hard error, or an OIDC job publishes an unsigned, attached-looking referrer.
        let mode = match opts.key.is_some() || token_provider.has_signing_material() {
            true => AttestMode::Signed,
            false => AttestMode::Unsigned,
        };
        let resolve = super::resolve_subject::sign_resolver(self.index(), resolved);
        let state = SigningStatePaths::new(self.file_structure().state.root());
        let context = AttestContext {
            identifier: package,
            platform,
            mode,
            format: opts.format,
            signer: signer.as_ref(),
            token_provider: &token_provider,
            predicate_type: &opts.predicate_type,
            predicate: &predicate,
            no_cache: opts.no_cache,
            offline: opts.offline,
            dial: self.index().dial_policy(package.registry()),
            resolve,
            fulcio_url: &opts.fulcio_url,
            rekor_url: &opts.rekor_url,
            state,
        };
        let result = AttestPipeline::run(client, context)
            .await
            .map_err(|err| map_attest_error(package.clone(), err))?;
        Ok(AttestReport { result })
    }
}

impl PackageManager {
    /// Attach the attestation to the index each of `tags` resolves to, under [`sign_tags`](Self::sign_tags)'s
    /// sweep rules: bare-manifest tags skipped, failures survived, never `Err`, one run per subject digest.
    pub async fn attest_tags(
        &self,
        package: &ocx_oci::PackageRef,
        tags: &[String],
        opts: &AttestOptions,
    ) -> Vec<super::sign::SweptTag<AttestReport>> {
        use super::sign::{SweptOutcome, SweptTag};
        use std::collections::HashMap;

        // Subject digest -> the tag whose run attested it.
        let mut attested: HashMap<ocx_oci::Digest, String> = HashMap::new();
        let mut swept = Vec::with_capacity(tags.len());
        for tag in tags {
            let identifier = package.clone_with_tag(tag.clone());
            let outcome = match self.resolve_swept_index(&identifier).await {
                Err(error) => SweptOutcome::Failed(Box::new(error)),
                Ok(None) => {
                    log::warn!("Skipping '{identifier}': it resolves to a single manifest, which push already signed.");
                    SweptOutcome::SkippedBareManifest
                }
                Ok(Some(resolved)) => match attested.get(&resolved.0).cloned() {
                    Some(first) => {
                        log::warn!(
                            "Skipping '{identifier}': it names the index tag '{first}' was already attested as; \
                             an attestation is a referrer of the subject digest, so one covers both."
                        );
                        SweptOutcome::CoveredBy(first)
                    }
                    None => match self.attest_one(&identifier, None, opts.clone(), Some(&resolved)).await {
                        Ok(report) => {
                            attested.insert(resolved.0.clone(), tag.clone());
                            SweptOutcome::Done(report)
                        }
                        Err(error) => SweptOutcome::Failed(Box::new(error)),
                    },
                },
            };
            swept.push(SweptTag {
                tag: tag.clone(),
                outcome,
            });
        }
        swept
    }
}

/// Wrap a [`SignError`] in a [`PackageError`] tagged with `identifier`, keeping the attest exit code.
fn map_attest_error(identifier: ocx_oci::PackageRef, err: SignError) -> PackageError {
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Sign(Box::new(err))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PackageErrorKind;
    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_sign::sign::SignErrorKind;
    use ocx_store::file_structure::FileStructure;

    /// A minimal offline manager — no OCI client, which is what `is_offline`
    /// reads.
    fn offline_manager(ocx_home: &std::path::Path) -> PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(ocx_home.join("index")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        PackageManager::new(fs, index, None, "localhost:5000")
    }

    fn options(predicate: &[u8]) -> AttestOptions {
        AttestOptions {
            key: None,
            rekor_upload: true,
            format: ocx_sign::sign::SignatureFormat::Bundle,
            fulcio_url: Url::parse("http://127.0.0.1:5555").expect("fulcio url"),
            rekor_url: Url::parse("http://127.0.0.1:3000").expect("rekor url"),
            identity_token: None,
            predicate_type: PredicateType::CycloneDx,
            predicate: predicate.to_vec(),
            no_cache: true,
            no_tty: true,
            offline: false,
        }
    }

    /// Reach the wrapped [`SignError`] by structure rather than by walking
    /// `source()`. `PackageError.kind` deliberately omits `#[source]`
    /// (`package_manager/error.rs`), so a `PackageError` has an empty source
    /// chain and a downcast walk finds nothing.
    ///
    /// Structure is also what the CLI reads: `package_sign.rs` unwraps exactly
    /// this shape before handing the error to anyhow, which is what makes the
    /// sign-side exit code survive. Asserting `classify_error` on the
    /// `PackageError` itself would assert a contract this layer does not hold
    /// — it answers `Failure` for every kind.
    fn sign_error(error: &PackageError) -> &SignError {
        let PackageErrorKind::Internal(crate::Error::Sign(sign)) = &error.kind else {
            panic!("expected an Internal(Sign(..)) kind, got: {:?}", error.kind);
        };
        sign
    }

    /// At the task layer, `require_client` answers `OfflineMode` (81) for
    /// an offline manager, so a task that reached for the client first would
    /// report a passive network failure where the contract says 77 policy
    /// refusal — and `ocx package push --sbom`, which never passes through
    /// `ocx package attest`'s own CLI gate, would get 81 with nothing else
    /// catching it.
    #[tokio::test]
    async fn attest_one_refuses_offline_as_a_policy_rejection_not_a_missing_client() {
        let temp = tempfile::TempDir::new().expect("ocx home");
        let manager = offline_manager(temp.path());
        let package = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").expect("identifier");

        let error = manager
            .attest_one(
                &package,
                Some(&ocx_oci::Platform::any()),
                AttestOptions {
                    offline: true,
                    ..options(br#"{"bomFormat":"CycloneDX"}"#)
                },
                None,
            )
            .await
            .expect_err("an offline attest must be refused");

        let sign = sign_error(&error);
        assert!(
            matches!(sign.kind, SignErrorKind::OfflineAttestRefused),
            "expected the offline policy refusal, got: {error}",
        );
        assert_eq!(&error.identifier, &package, "the refusal is tagged with the target");
    }

    /// The CLI hands over raw bytes, and the task layer is
    /// where they become the `RawValue` the pipeline splices. Non-JSON bytes
    /// are a malformed *file* (65), not a bad invocation.
    #[tokio::test]
    async fn attest_one_refuses_a_predicate_that_is_not_json() {
        let temp = tempfile::TempDir::new().expect("ocx home");
        let manager = offline_manager(temp.path());
        let package = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").expect("identifier");

        for bytes in [b"not json at all".to_vec(), vec![0xff, 0xfe, 0xfd]] {
            let error = manager
                .attest_one(&package, Some(&ocx_oci::Platform::any()), options(&bytes), None)
                .await
                .expect_err("a non-JSON predicate must be refused");

            let sign = sign_error(&error);
            assert!(
                matches!(sign.kind, SignErrorKind::PredicateNotJson),
                "expected the predicate refusal, got: {error}",
            );
        }
    }

    /// The attest sweep resolves each tag once too.
    ///
    /// The issue names only `sign_tags`, but `attest_tags` imports the same
    /// `resolve_swept_index` and the same `resolve_platform_target`, so it
    /// carried the identical 2N multiplier. Asserted separately rather than
    /// assumed from the sign test: the two sweeps are two call sites, and one
    /// of them could be threaded while the other was not.
    #[tokio::test]
    async fn an_attest_tag_sweep_resolves_each_tag_exactly_once() {
        use std::sync::{Arc, Mutex};

        use super::super::sign::SweptOutcome;
        use super::super::sign::sweep_test_support::{
            TAGS, expected_resolutions, manifest_reads, sweep_identifier, sweep_manager,
        };

        let asked = Arc::new(Mutex::new(Vec::new()));
        let temp = tempfile::TempDir::new().expect("ocx home");
        let (manager, transport) = sweep_manager(Arc::clone(&asked), temp.path());
        let tags: Vec<String> = TAGS.iter().map(|tag| (*tag).to_string()).collect();

        let swept = manager
            .attest_tags(&sweep_identifier(), &tags, &options(br#"{"bomFormat":"CycloneDX"}"#))
            .await;

        assert_eq!(swept.len(), TAGS.len(), "one row per swept tag");
        for row in &swept {
            let outcome = match &row.outcome {
                SweptOutcome::Done(_) => "attested",
                SweptOutcome::SkippedBareManifest => "skipped",
                SweptOutcome::CoveredBy(_) => "covered",
                SweptOutcome::Failed(_) => "failed",
            };
            assert_eq!(
                outcome, "failed",
                "the empty registry fails each tag inside the pipeline; a skip would mean \
                 the sweep never entered it. `covered` would mean worse: every tag here \
                 resolves to one digest, so recording a digest whose run *failed* would \
                 report these siblings as covered by an attestation nobody published, and \
                 they must be retried instead (tag '{}')",
                row.tag,
            );
        }
        assert_eq!(
            manifest_reads(&transport),
            TAGS.len(),
            "positive control: each tag reached the pipeline's subject fetch",
        );
        assert_eq!(
            *asked.lock().expect("asked lock"),
            expected_resolutions(),
            "one manifest resolution per swept tag, each for that tag",
        );
    }
}
