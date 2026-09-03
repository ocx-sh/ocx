// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `attest_one` — package-manager task that attaches one in-toto attestation
//! to a single target manifest.
//!
//! Mirrors [`sign_one`](super::sign) exactly: the client and index come from
//! the [`PackageManager`] facade, the pipeline's [`AttestResult`] becomes an
//! [`AttestReport`], and any failure is wrapped in a [`PackageError`] tagged
//! with the target identifier.
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — tasks live in `package_manager/tasks/`; the aggregator is
//! `package_manager/tasks.rs` (not `tasks/mod.rs`).

use url::Url;
use zeroize::Zeroizing;

use crate::oci;
use crate::oci::attest::pipeline::{AttestContext, AttestMode, AttestPipeline, AttestResult};
use crate::oci::attest::predicate::PredicateType;
use crate::oci::sign::{DispatchingTokenProvider, SignError, SignErrorKind};
use crate::package_manager::error::{PackageError, PackageErrorKind};

use super::super::PackageManager;

/// Options forwarded from the CLI to [`PackageManager::attest_one`].
///
/// Mirrors [`SignOptions`](super::sign::SignOptions), plus the two fields
/// attesting adds and the `offline` policy flag signing keeps at its own CLI
/// boundary.
///
/// `Clone` for the same reason [`SignOptions`](super::sign::SignOptions) is: a
/// `--tags` / `--tags-file` sweep attests N references from one parsed option
/// set.
#[derive(Clone)]
pub struct AttestOptions {
    /// Fulcio CA endpoint (validated by the CLI). Default: `https://fulcio.sigstore.dev`.
    pub fulcio_url: Url,
    /// Rekor transparency log endpoint (validated by the CLI). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: Url,
    /// OIDC override token (file / stdin / env, resolved by the CLI layer).
    pub identity_token: Option<Zeroizing<String>>,
    /// The requested `--type`; its resolved URI is what gets written (D-c).
    pub predicate_type: PredicateType,
    /// RAW FILE BYTES, not a parsed `Value`. Validated by a parse whose result
    /// is discarded, then spliced verbatim (D-b). A `Value` here would
    /// normalize whitespace and number spelling before anything downstream
    /// could preserve them.
    pub predicate: Vec<u8>,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// When true, suppress the browser OAuth fallback (CI / headless).
    pub no_tty: bool,
    /// Mirrors the S1-E policy: the refusal runs before token resolution.
    pub offline: bool,
    /// Selects key mode. `None` is keyless — see [`SignOptions::key`](super::sign::SignOptions::key).
    pub key: Option<oci::sign::KeyRef>,
    /// Whether a transparency-log entry is uploaded — see
    /// [`SignOptions::rekor_upload`](super::sign::SignOptions::rekor_upload).
    pub rekor_upload: bool,
    /// Which cosign wire shape(s) to publish the attestation in — see
    /// [`AttestContext::format`](crate::oci::attest::pipeline::AttestContext::format).
    pub format: oci::sign::SignatureFormat,
}

/// Success payload returned by [`PackageManager::attest_one`].
#[derive(Debug)]
pub struct AttestReport {
    /// Raw pipeline result (subject digest, resolved predicate type, referrer descriptor).
    pub result: AttestResult,
}

impl PackageManager {
    /// Attach an in-toto attestation to what `package` resolves to, publishing
    /// a DSSE-enveloped Sigstore bundle v0.3 referrer manifest.
    ///
    /// `platform` narrows exactly as it does for
    /// [`sign_one`](Self::sign_one): `None` attests the resolved object,
    /// `Some` narrows into an index to that child.
    ///
    /// # Errors
    ///
    /// [`PackageError`] tagged with `package` on any failure — exit-code
    /// classification routes via [`crate::oci::sign::SignErrorKind`].
    pub async fn attest_one(
        &self,
        package: &oci::Identifier,
        platform: Option<&oci::Platform>,
        opts: AttestOptions,
        resolved: Option<&(oci::Digest, oci::Manifest)>,
    ) -> Result<AttestReport, PackageError> {
        // The S1-E refusal has to answer here as well as in the pipeline:
        // `require_client` below reports `OfflineMode` (81) for an offline
        // manager, which would shadow the 77 policy code a script branches on
        // to tell a deliberate refusal from an outage. The pipeline keeps its
        // own check for every other caller; this one keeps 81 from winning.
        if opts.offline {
            return Err(map_attest_error(
                package.clone(),
                SignError::new(package.clone(), SignErrorKind::OfflineAttestRefused),
            ));
        }

        // The CLI hands over raw file bytes on purpose (D-b): parsing to a
        // `Value` here would normalize whitespace and number spelling before
        // anything downstream could preserve them. `RawValue` validates the
        // bytes as JSON and keeps the original slice, which is what gets
        // signed.
        let predicate: Box<serde_json::value::RawValue> = serde_json::from_slice(&opts.predicate).map_err(|_| {
            // The parse error itself is discarded: it quotes the offending
            // bytes, which came from a file the user named and would reach
            // the terminal unsanitized.
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
        // Polarity: sign iff a signing identity is *visible*. An override token
        // or a detected ambient CI identity means signed, and a failure to
        // redeem it stays a hard error — a downgrade there would publish an
        // identity-less artifact from a job configured for OIDC, and the
        // referrer would look attached either way. Only the total absence of
        // signing material reaches the unsigned attach, which is where both
        // verbs used to dead-end at exit 77.
        //
        // Key mode short-circuits it: `--key` IS the signing material, so an
        // attach that named a key must never degrade to an unsigned one because
        // no OIDC identity happened to be around.
        let mode = match opts.key.is_some() || token_provider.has_signing_material() {
            true => AttestMode::Signed,
            false => AttestMode::Unsigned,
        };
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
            index: self.index(),
            resolved,
            fulcio_url: &opts.fulcio_url,
            rekor_url: &opts.rekor_url,
            state: &self.file_structure().state,
        };
        let result = AttestPipeline::run(client, context)
            .await
            .map_err(|err| map_attest_error(package.clone(), err))?;
        Ok(AttestReport { result })
    }
}

impl PackageManager {
    /// Attach the attestation to the index each of `tags` resolves to, in the
    /// repository `package` names.
    ///
    /// The index sweep [`sign_tags`](Self::sign_tags) performs, for the attest
    /// verb: same skip rule (a tag resolving to a bare manifest is left alone),
    /// same survive-and-continue rule, same never-`Err` contract, and the same
    /// one-run-per-distinct-subject-digest rule: an attestation is a referrer
    /// of the subject digest too, so a cascade release's aliases collapse to
    /// one run here exactly as they do for `sign`. One predicate is attached to
    /// every swept index — the predicate is the caller's file, read once before
    /// the sweep starts.
    pub async fn attest_tags(
        &self,
        package: &oci::Identifier,
        tags: &[String],
        opts: &AttestOptions,
    ) -> Vec<super::sign::SweptTag<AttestReport>> {
        use super::sign::{SweptOutcome, SweptTag};
        use std::collections::HashMap;

        // Subject digest -> the tag whose run attested it, exactly as in
        // `sign_tags`: see [`SweptOutcome::CoveredBy`].
        let mut attested: HashMap<oci::Digest, String> = HashMap::new();
        let mut swept = Vec::with_capacity(tags.len());
        for tag in tags {
            let identifier = package.clone_with_tag(tag.clone());
            let outcome = match self.resolve_swept_index(&identifier).await {
                Err(error) => SweptOutcome::Failed(Box::new(error)),
                Ok(None) => {
                    crate::log::warn!(
                        "Skipping '{identifier}': it resolves to a single manifest, which push already signed."
                    );
                    SweptOutcome::SkippedBareManifest
                }
                // The resolution travels on, exactly as it does in
                // `sign_tags`: this loop already asked the index chain what the
                // tag names, and the pipeline would ask again (#373).
                //
                // `.cloned()` ends the borrow before the `None` arm inserts.
                Ok(Some(resolved)) => match attested.get(&resolved.0).cloned() {
                    Some(first) => {
                        crate::log::warn!(
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

/// Wrap a [`SignError`] in a [`PackageError`] tagged with `identifier`,
/// preserving the attest exit code through `PackageErrorKind::Internal`.
fn map_attest_error(identifier: oci::Identifier, err: SignError) -> PackageError {
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Sign(Box::new(err))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ExitCode, classify_error};
    use crate::file_structure::{FileStructure, IndexStore};
    use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
    use crate::oci::sign::SignErrorKind;
    use crate::package_manager::error::PackageErrorKind;

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
            format: oci::sign::SignatureFormat::Bundle,
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

    /// S-002 at the task layer. `require_client` answers `OfflineMode` (81) for
    /// an offline manager, so a task that reached for the client first would
    /// report a passive network failure where the contract says 77 policy
    /// refusal — and `ocx package push --sbom`, which never passes through
    /// `ocx package attest`'s own CLI gate, would get 81 with nothing else
    /// catching it.
    #[tokio::test]
    async fn attest_one_refuses_offline_as_a_policy_rejection_not_a_missing_client() {
        let temp = tempfile::TempDir::new().expect("ocx home");
        let manager = offline_manager(temp.path());
        let package = crate::oci::Identifier::parse("registry.example/pkg:1.0").expect("identifier");

        let error = manager
            .attest_one(
                &package,
                Some(&crate::oci::Platform::any()),
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
        assert_eq!(classify_error(sign), ExitCode::PermissionDenied);
        assert_eq!(&error.identifier, &package, "the refusal is tagged with the target");
    }

    /// S-005 read half: the CLI hands over raw bytes, and the task layer is
    /// where they become the `RawValue` the pipeline splices. Non-JSON bytes
    /// are a malformed *file* (65), not a bad invocation.
    #[tokio::test]
    async fn attest_one_refuses_a_predicate_that_is_not_json() {
        let temp = tempfile::TempDir::new().expect("ocx home");
        let manager = offline_manager(temp.path());
        let package = crate::oci::Identifier::parse("registry.example/pkg:1.0").expect("identifier");

        for bytes in [b"not json at all".to_vec(), vec![0xff, 0xfe, 0xfd]] {
            let error = manager
                .attest_one(&package, Some(&crate::oci::Platform::any()), options(&bytes), None)
                .await
                .expect_err("a non-JSON predicate must be refused");

            let sign = sign_error(&error);
            assert!(
                matches!(sign.kind, SignErrorKind::PredicateNotJson),
                "expected the predicate refusal, got: {error}",
            );
            assert_eq!(classify_error(sign), ExitCode::DataError);
        }
    }

    /// **S-011 / C-041.** The attest sweep resolves each tag once too.
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
