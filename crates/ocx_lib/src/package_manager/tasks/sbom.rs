// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `sbom_one` — list every verified attestation a package carries.
//!
//! The attestation twin of [`super::verify`]: same pipeline, same trust
//! material, different arity. [`crate::oci::verify::VerifyPipeline::run`] is
//! ANY-of because *is this artifact signed* has one answer;
//! [`crate::oci::verify::VerifyPipeline::run_attestations`] is collect-all
//! because *which SBOMs does this carry* does not — letting the registry's
//! listing order pick one of several verified documents would be the defect
//! (`adr_sbom_attestations.md` D-e).
//!
//! Verification is unconditional and there is no `--no-verify`: an unverified
//! listing is registry-controlled text presented as fact (SEC-32).
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — the aggregator is `package_manager/tasks.rs`.

use url::Url;

use crate::file_structure::StateStore;
use crate::oci::attest::predicate::PredicateType;
use crate::oci::verify::{
    AttestationMatch, AttestationScan, RefusedCandidate, TrustRoot, UnverifiedSbom, VerificationMode,
    VerifyContentMode, VerifyContext, VerifyPipeline,
};
use crate::oci::{self};
use crate::package_manager::error::PackageError;
use crate::trust::CompiledPolicy;

use super::super::PackageManager;
use super::verify::map_verify_error;

/// External dependencies forwarded to [`PackageManager::sbom_one`].
///
/// Mirrors [`VerifyOptions`](super::verify::VerifyOptions) field for field,
/// plus the `--type` narrowing. The content mode is not a field: this task
/// verifies attestations by definition, so a `Signature` mode here would be an
/// unrepresentable request rather than a configurable one.
pub struct SbomOptions<'a> {
    /// Resolved ANY-of policies the signing certificate must satisfy. Empty
    /// under [`VerificationMode::Permissive`], where nothing is verified.
    pub policies: &'a [CompiledPolicy],
    /// Whether this run demands verification, resolved at the CLI boundary
    /// from the flags and the trust policies.
    pub verification: VerificationMode,
    /// Registry client (always available, unlike the manager's offline client).
    pub client: &'a oci::Client,
    /// Trust root (Fulcio CA + optional pinned Rekor key); #196 seam.
    pub trust_root: &'a TrustRoot,
    /// Rekor transparency-log endpoint (default public Rekor).
    pub rekor_url: &'a Url,
    /// When true, no Sigstore-trust-services network — the Rekor key must come
    /// from pinned/cached trust material.
    pub offline: bool,
    /// State store owning the referrers-capability and trust-root cache layouts.
    pub state: &'a StateStore,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// `--type` narrowing; `None` lists every verified attestation. Narrowing
    /// is applied to the **signed** predicateType after fetch-and-parse —
    /// annotations never exclude a candidate (D-e).
    pub predicate_type: Option<PredicateType>,
}

/// Success payload returned by [`PackageManager::sbom_one`].
///
/// Carries the refusals beside the matches. A scan that returns matches has
/// usually also looked at candidates that failed, and dropping those makes
/// "3 attestations" indistinguishable from "3 attestations, 2 refused" — the
/// second is the one worth acting on, so the caller reports both (WP6 ruling).
pub struct SbomReport {
    /// Every attestation that verified, in listing order.
    pub attestations: Vec<AttestationMatch>,
    /// Every SBOM attached **without** a signature, in digest order. Carried
    /// separately so a caller cannot present one as the other.
    pub unverified: Vec<UnverifiedSbom>,
    /// Every candidate that was examined and refused, in listing order.
    pub refused: Vec<RefusedCandidate>,
}

impl PackageManager {
    /// List every verified attestation on `package` for `platform`, narrowed to
    /// `opts.predicate_type` when set.
    ///
    /// Read-only: routes through [`read_only_view`](Self::read_only_view) like
    /// [`verify_one`](Self::verify_one), so reading an SBOM never grows the
    /// permanent local index.
    ///
    /// # Errors
    /// Returns [`PackageError`] tagged with `package` on any failure —
    /// exit-code classification routes via
    /// [`crate::oci::verify::VerifyErrorKind`]. A scan that ends with no match
    /// is `AttestationNotFound` (79), including the `--type` narrowing miss.
    pub async fn sbom_one(
        &self,
        package: &oci::Identifier,
        platform: &oci::Platform,
        opts: SbomOptions<'_>,
    ) -> Result<SbomReport, PackageError> {
        // Read-only, for the same reason `verify_one` is: reading what a
        // package carries must never grow the permanent local index
        // (`adr_index_indirection.md`). Content still warms the GC-able blob
        // cache.
        let mgr = self.read_only_view();
        let context = VerifyContext {
            identifier: package,
            platform,
            policies: opts.policies,
            no_cache: opts.no_cache,
            index: mgr.index(),
            trust_root: opts.trust_root,
            rekor_url: opts.rekor_url,
            state: opts.state,
            offline: opts.offline,
            content: VerifyContentMode::Attestation {
                predicate_type: opts.predicate_type,
            },
            verification: opts.verification,
        };
        let scan = VerifyPipeline::run_attestations(opts.client, context)
            .await
            .map_err(|err| map_verify_error(package.clone(), err))?;
        Ok(report_from(scan))
    }
}

/// Carry a finished scan into the report shape, refusals included.
///
/// Named rather than inlined so the "refusals are never dropped" contract has
/// one place to assert against: a scan is expensive to reach through the
/// pipeline, and this is the step that could silently lose half of it.
fn report_from(scan: AttestationScan) -> SbomReport {
    // Destructured, not field-read: a third field on `AttestationScan` must
    // break this build rather than be dropped on the floor here.
    let AttestationScan {
        matches,
        unverified,
        refused,
    } = scan;
    SbomReport {
        attestations: matches,
        unverified,
        refused,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::value::RawValue;
    use tempfile::TempDir;
    use url::Url;

    use super::*;
    use crate::cli::ExitCode;
    use crate::file_structure::{FileStructure, StateStore};
    use crate::oci::index::{ChainMode, Index, IndexOperation, LocalConfig, LocalIndex};
    use crate::oci::verify::pipeline::VerifyResult;
    use crate::oci::verify::{VerifiedAttestation, VerifyError, VerifyErrorKind};
    use crate::package_manager::error::PackageErrorKind;
    use crate::sbom::cyclonedx::summarize_cyclonedx;

    use super::super::verify::tests::{
        REGISTRY, REPO, SingleTagSource, index_digest, seed_unsupported_capability, tagged_id,
    };

    /// A CycloneDX 1.6 predicate as a publisher would have signed it —
    /// pretty-printed, which is exactly the shape `--output` must not reflow.
    const CYCLONEDX_1_6: &str = r#"{
  "bomFormat": "CycloneDX",
  "specVersion": "1.6",
  "serialNumber": "urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79",
  "metadata": { "component": { "type": "application", "name": "widget" } },
  "components": [
    { "type": "library", "name": "left-pad" },
    { "type": "library", "name": "right-pad" }
  ]
}"#;

    fn digest_of(bytes: &[u8]) -> oci::Digest {
        oci::Algorithm::Sha256.hash(bytes)
    }

    fn attestation(predicate: &str, predicate_type: &str) -> VerifiedAttestation {
        VerifiedAttestation {
            predicate_type: predicate_type.to_string(),
            payload: br#"{"_type":"https://in-toto.io/Statement/v1"}"#.to_vec(),
            predicate: RawValue::from_string(predicate.to_string()).unwrap(),
            subject_digest: digest_of(b"subject"),
        }
    }

    fn a_match(referrer: &str, predicate: &str, predicate_type: &str) -> AttestationMatch {
        AttestationMatch {
            verify: VerifyResult {
                subject_digest: digest_of(b"subject"),
                referrer_digest: digest_of(referrer.as_bytes()),
                certificate_identity: "https://github.com/acme/widget/.github/workflows/release.yml@refs/heads/main"
                    .to_string(),
                certificate_oidc_issuer: "https://token.actions.githubusercontent.com".to_string(),
                signed_at: 1_700_000_000,
            },
            attestation: attestation(predicate, predicate_type),
        }
    }

    fn a_refusal(referrer: &str, reason: VerifyErrorKind) -> RefusedCandidate {
        RefusedCandidate {
            referrer_digest: referrer.to_string(),
            reason,
        }
    }

    /// S-006: the listing is ALL verified attestations, in listing order —
    /// and the refusals travel with them. Dropping either half makes
    /// "2 attestations" indistinguishable from "2 attestations, 2 refused",
    /// which is the one worth acting on (WP6 ruling).
    #[test]
    fn report_carries_every_match_and_every_refusal_in_listing_order() {
        let scan = AttestationScan {
            matches: vec![
                a_match("first", CYCLONEDX_1_6, "https://cyclonedx.org/bom"),
                a_match("second", r#"{"specVersion":"1.5"}"#, "https://cyclonedx.org/bom"),
            ],
            unverified: Vec::new(),
            refused: vec![
                a_refusal("sha256:aa", VerifyErrorKind::BundleParseFailed),
                a_refusal("sha256:bb", VerifyErrorKind::MultipleSignatures { count: 2 }),
            ],
        };

        let report = report_from(scan);

        let referrers: Vec<String> = report
            .attestations
            .iter()
            .map(|entry| entry.verify.referrer_digest.to_string())
            .collect();
        assert_eq!(
            referrers,
            vec![digest_of(b"first").to_string(), digest_of(b"second").to_string()],
            "every match must survive, in listing order"
        );
        let refused: Vec<&str> = report
            .refused
            .iter()
            .map(|candidate| candidate.referrer_digest.as_str())
            .collect();
        assert_eq!(
            refused,
            vec!["sha256:aa", "sha256:bb"],
            "every refusal must survive: a report that drops them under-reports silently"
        );
        assert!(
            matches!(
                report.refused[1].reason,
                VerifyErrorKind::MultipleSignatures { count: 2 }
            ),
            "the refusal reason is what the caller prints; got {:?}",
            report.refused[1].reason
        );
    }

    /// A scan with matches and no refusals reports no refusals — the
    /// positive control for the assertion above, so "always empty" and
    /// "correctly empty" are distinguishable.
    #[test]
    fn report_with_no_refusals_is_empty_not_fabricated() {
        let scan = AttestationScan {
            matches: vec![a_match("only", CYCLONEDX_1_6, "https://cyclonedx.org/bom")],
            unverified: Vec::new(),
            refused: Vec::new(),
        };
        let report = report_from(scan);
        assert_eq!(report.attestations.len(), 1);
        assert!(report.refused.is_empty());
    }

    /// S-007/S-019: the predicate a match carries is the verbatim signed
    /// sub-slice, so it feeds `crate::sbom` directly. This is the seam the
    /// CLI's `--summary` and `--output` both stand on — if `AttestationMatch`
    /// ever carried a re-serialization, `--output` would stop being
    /// byte-identical and this summary would still pass, so assert the bytes
    /// too.
    #[test]
    fn match_predicate_bytes_are_verbatim_and_summarize() {
        let report = report_from(AttestationScan {
            matches: vec![a_match("only", CYCLONEDX_1_6, "https://cyclonedx.org/bom")],
            unverified: Vec::new(),
            refused: Vec::new(),
        });
        let predicate = report.attestations[0].attestation.predicate.get();
        assert_eq!(
            predicate, CYCLONEDX_1_6,
            "the predicate must be the verbatim signed bytes"
        );

        let summary = summarize_cyclonedx(predicate.as_bytes()).expect("CycloneDX 1.6 must summarize");
        assert_eq!(summary.spec_version, "1.6");
        assert_eq!(summary.component_count, 2);
        assert_eq!(summary.top_level_component.as_deref(), Some("widget"));
        assert_eq!(
            summary.serial_number.as_deref(),
            Some("urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79")
        );
    }

    /// S-017: a scan that ends with no match — including the `--type`
    /// narrowing miss — is `AttestationNotFound`, and the task's error wrapper
    /// must carry that all the way to exit 79. A wrapper that flattened the
    /// kind would classify as a generic failure and every `case $?` in a
    /// consumer script would break.
    #[test]
    fn attestation_not_found_survives_the_task_wrapper_as_exit_79() {
        let error = super::super::verify::map_verify_error(
            tagged_id(),
            VerifyError::new(tagged_id(), VerifyErrorKind::AttestationNotFound),
        );
        // `PackageError` carries no `#[source]`, so the exit code survives
        // only because the wrapper keeps the `VerifyError` whole inside
        // `Internal` for the CLI boundary to re-root the chain on
        // (`verify_error_into_anyhow`). A wrapper that flattened the kind
        // would classify as a generic failure and every `case $?` in a
        // consumer script would break.
        let inner = match error.kind {
            PackageErrorKind::Internal(crate::Error::Verify(verify_error)) => verify_error,
            other => panic!("expected Internal(Verify(AttestationNotFound)), got {other:?}"),
        };
        assert!(matches!(inner.kind, VerifyErrorKind::AttestationNotFound));
        assert_eq!(
            crate::cli::classify_error(inner.as_ref() as &(dyn std::error::Error + 'static)),
            ExitCode::NotFound
        );

        // Discriminating control: a different verify kind classifies
        // differently through the identical wrapper, so the assertion above
        // cannot be green for a wrapper that returns one fixed code.
        let other = super::super::verify::map_verify_error(
            tagged_id(),
            VerifyError::new(tagged_id(), VerifyErrorKind::MultipleSignatures { count: 2 }),
        );
        let other_inner = match other.kind {
            PackageErrorKind::Internal(crate::Error::Verify(verify_error)) => verify_error,
            other => panic!("expected Internal(Verify(..)), got {other:?}"),
        };
        assert_eq!(
            crate::cli::classify_error(other_inner.as_ref() as &(dyn std::error::Error + 'static)),
            ExitCode::DataError
        );
    }

    /// `sbom_one` drives the attestation scan through the same pipeline as
    /// `verify_one` and wraps its failures identically — proven on the one
    /// pipeline outcome a unit test can reach without live Sigstore material.
    /// (The verified-listing paths are acceptance-tested: WP10a.)
    #[tokio::test(flavor = "multi_thread")]
    async fn sbom_one_wraps_pipeline_failures_and_never_grows_the_local_index() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let root = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(root.path().to_path_buf());
        let index_store = file_structure.index.clone();
        let local_index = LocalIndex::new(LocalConfig {
            index_store: index_store.clone(),
        });
        let source = Index::from_impl(SingleTagSource);
        let index = Index::from_chained(local_index, vec![source], ChainMode::Default);
        let manager = PackageManager::new(file_structure, index, None, REGISTRY);

        let state = StateStore::new(root.path().join("state"));
        seed_unsupported_capability(&state).await;

        let client = oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let trust_root = TrustRoot::default();
        let rekor_url = Url::parse("http://127.0.0.1:3000").unwrap();
        let platform: oci::Platform = "linux/amd64".parse().unwrap();

        let opts = SbomOptions {
            policies: &[],
            client: &client,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            offline: false,
            state: &state,
            no_cache: false,
            predicate_type: None,
            verification: VerificationMode::Demand,
        };

        match manager.sbom_one(&tagged_id(), &platform, opts).await {
            Err(err) => match err.kind {
                PackageErrorKind::Internal(crate::Error::Verify(verify_err)) => assert!(
                    matches!(verify_err.kind, VerifyErrorKind::ReferrersUnsupported),
                    "expected ReferrersUnsupported, got {:?}",
                    verify_err.kind
                ),
                other => panic!("expected Internal(Verify(ReferrersUnsupported)), got {other:?}"),
            },
            Ok(_) => panic!("no referrers support configured; sbom_one must fail closed"),
        }

        // Read-only routing: listing a package's SBOMs must not grow the
        // permanent local index, exactly as `verify_one` must not. The
        // writable positive control for this fixture lives in the verify
        // task's own test.
        assert!(
            !index_store
                .dispatch_object_path(REGISTRY, REPO, &index_digest())
                .exists(),
            "sbom_one must not persist a dispatch object into the local index"
        );
        let offline_probe = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: index_store.clone(),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        assert!(
            offline_probe
                .fetch_manifest(&tagged_id(), IndexOperation::Query)
                .await
                .unwrap()
                .is_none(),
            "sbom_one must not commit a tag pointer; an offline probe found one"
        );
    }
}
