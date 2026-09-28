// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `sbom_one` — list every verified attestation a package carries.
//!
//! Collect-all via [`ocx_sign::verify::VerifyPipeline::run_attestations`], never ANY-of, or the registry's
//! listing order picks one of several verified documents (`adr_sbom_attestations.md` D-e).
//! See [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md).

use std::collections::BTreeSet;

use url::Url;

use crate::error::PackageError;
use ocx_oci::{self};
use ocx_sign::attest::predicate::PredicateType;
use ocx_sign::verify::{
    AttestationMatch, AttestationScan, RefusedCandidate, TrustRoot, UnverifiedSbom, VerificationMode,
    VerifyContentMode, VerifyContext, VerifyPipeline,
};
use ocx_store::file_structure::StateStore;
use ocx_trust::CompiledPolicy;

use super::super::PackageManager;
use super::verify::map_verify_error;

/// External dependencies forwarded to [`PackageManager::sbom_one`].
pub struct SbomOptions<'a> {
    /// Resolved ANY-of policies; empty under [`VerificationMode::Permissive`], where nothing is verified.
    pub policies: &'a [CompiledPolicy],
    /// Whether this run demands verification, resolved at the CLI boundary.
    pub verification: VerificationMode,
    /// Registry client, present even under `--offline`.
    pub client: &'a ocx_oci::Client,
    /// Trust root (Fulcio CA + optional pinned Rekor key).
    pub trust_root: &'a TrustRoot,
    /// Rekor transparency-log endpoint (default public Rekor).
    pub rekor_url: &'a Url,
    /// No Sigstore trust-service network; the Rekor key must come from pinned or cached trust material.
    pub offline: bool,
    /// State store owning the referrers-capability and trust-root cache layouts.
    pub state: &'a StateStore,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// `--type` narrowing on the signed predicateType, never on annotations; `None` lists every attestation.
    pub predicate_type: Option<PredicateType>,
    /// The cosign wire shape this run pins (`--signature-format`).
    pub signature_format: Option<ocx_sign::sign::SignatureFormat>,
}

/// Success payload returned by [`PackageManager::sbom_one`]; refusals ride beside the matches so the
/// caller can report both.
pub struct SbomReport {
    /// Every attestation that verified, in listing order.
    pub attestations: Vec<AttestationMatch>,
    /// Every SBOM attached without a signature, in digest order, kept apart so a caller cannot present
    /// one as the other.
    pub unverified: Vec<UnverifiedSbom>,
    /// Every candidate that was examined and refused, in listing order.
    pub refused: Vec<RefusedCandidate>,
    /// Referrer digests of the documents a platform-level SBOM of the same predicateType supersedes.
    ///
    /// Keyed on the referrer digest, the one per-document key: the same bytes on two subjects are two referrers.
    pub shadowed: BTreeSet<ocx_oci::Digest>,
}

impl PackageManager {
    /// List every verified attestation on `package` for `platform` (as on [`verify_one`](Self::verify_one)),
    /// narrowed to `opts.predicate_type` when set.
    ///
    /// # Errors
    /// A [`PackageError`] tagged with `package`; a scan with no match, `--type` miss included, is
    /// `AttestationNotFound` (79).
    pub async fn sbom_one(
        &self,
        package: &ocx_oci::PackageRef,
        platform: Option<&ocx_oci::Platform>,
        opts: SbomOptions<'_>,
    ) -> Result<SbomReport, PackageError> {
        // Read-only view: reading must never grow the permanent local index (`adr_index_indirection.md`).
        let mgr = self.read_only_view();
        let resolve = super::resolve_subject::verify_resolver(mgr.index());
        let state = ocx_sign::sign::state::SigningStatePaths::new(opts.state.root());
        let context = VerifyContext {
            identifier: package,
            platform,
            policies: opts.policies,
            no_cache: opts.no_cache,
            dial: mgr.index().dial_policy(package.registry()),
            resolve,
            trust_root: opts.trust_root,
            rekor_url: opts.rekor_url,
            state,
            offline: opts.offline,
            content: VerifyContentMode::Attestation {
                predicate_type: opts.predicate_type,
            },
            verification: opts.verification,
            signature_format: opts.signature_format,
            // Not inert: gates a keyless `.att` like a `.sig`, and `sbom` has no opt-out flag.
            allow_unlogged_signature: false,
            report_all: true,
        };
        let scan = VerifyPipeline::run_attestations(opts.client, context)
            .await
            .map_err(|err| map_verify_error(package.clone(), err))?;
        Ok(report_from(scan))
    }
}

/// Carry a finished scan into the report shape, refusals included.
fn report_from(scan: AttestationScan) -> SbomReport {
    // Destructured so a new `AttestationScan` field breaks this build instead of being dropped.
    let AttestationScan {
        matches,
        unverified,
        refused,
        platform_subject,
    } = scan;
    let shadowed = shadowed_documents(platform_subject.as_ref(), &matches, &unverified);
    SbomReport {
        attestations: matches,
        unverified,
        refused,
        shadowed,
    }
}

/// Which documents a platform-level SBOM supersedes, per `predicateType`, or a platform CycloneDX hides an
/// index-level SPDX.
fn shadowed_documents(
    platform_subject: Option<&ocx_oci::Digest>,
    matches: &[AttestationMatch],
    unverified: &[UnverifiedSbom],
) -> BTreeSet<ocx_oci::Digest> {
    let Some(platform_subject) = platform_subject else {
        return BTreeSet::new();
    };
    // `verify.subject_digest` is where the scan found the referrer, comparable to `platform_subject`; the
    // statement's claim about itself is not.
    let documents = matches
        .iter()
        .map(|candidate| {
            (
                &candidate.verify.subject_digest,
                candidate.attestation.predicate_type.as_str(),
                &candidate.verify.referrer_digest,
            )
        })
        .chain(unverified.iter().map(|candidate| {
            (
                &candidate.subject_digest,
                candidate.predicate_type.as_str(),
                &candidate.referrer_digest,
            )
        }));

    let (platform_level, index_level): (Vec<_>, Vec<_>) =
        documents.partition(|(subject, ..)| *subject == platform_subject);
    let superseding: BTreeSet<&str> = platform_level
        .into_iter()
        .map(|(_, predicate_type, _)| predicate_type)
        .collect();
    index_level
        .into_iter()
        .filter(|(_, predicate_type, _)| superseding.contains(predicate_type))
        .map(|(_, _, referrer_digest)| referrer_digest.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::value::RawValue;
    use tempfile::TempDir;
    use url::Url;

    use super::*;
    use crate::error::PackageErrorKind;
    use ocx_index::{ChainMode, Index, IndexOperation, LocalConfig, LocalIndex};
    use ocx_sign::sbom::cyclonedx::summarize_cyclonedx;
    use ocx_sign::verify::pipeline::VerifyResult;
    use ocx_sign::verify::{VerifiedAttestation, VerifyError, VerifyErrorKind};
    use ocx_store::file_structure::{FileStructure, StateStore};

    use super::super::verify::tests::{
        REGISTRY, REPO, SingleTagSource, index_digest, tagged_id, transport_without_referrers,
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

    fn digest_of(bytes: &[u8]) -> ocx_oci::Digest {
        ocx_oci::Algorithm::Sha256.hash(bytes)
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
                key_backend: ocx_trust::key_ref::KeyBackendKind::Keyless,
                certificate_identity: Some(
                    "https://github.com/acme/widget/.github/workflows/release.yml@refs/heads/main".to_string(),
                ),
                certificate_oidc_issuer: Some("https://token.actions.githubusercontent.com".to_string()),
                signed_at: Some(1_700_000_000),
                signature_format: ocx_sign::sign::SignatureFormat::Bundle,
                discovery_method: ocx_sign::verify::DiscoveryMethod::ReferrersApi,
                rekor_log_index: None,
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

    // ── Shadowing ────────────────────────────────────────────────────────────

    const CYCLONEDX: &str = "https://cyclonedx.org/bom";
    const SPDX: &str = "https://spdx.dev/Document";

    /// The per-platform manifest `--platform` narrowed to.
    fn platform_subject() -> ocx_oci::Digest {
        digest_of(b"the platform manifest")
    }

    /// The image index that manifest was reached through.
    fn enclosing_subject() -> ocx_oci::Digest {
        digest_of(b"the enclosing image index")
    }

    fn a_match_on(subject: &ocx_oci::Digest, referrer: &str, predicate_type: &str) -> AttestationMatch {
        let mut candidate = a_match(referrer, CYCLONEDX_1_6, predicate_type);
        candidate.verify.subject_digest = subject.clone();
        candidate.attestation.subject_digest = subject.clone();
        candidate
    }

    fn an_unverified_on(subject: &ocx_oci::Digest, referrer: &str, predicate_type: &str) -> UnverifiedSbom {
        UnverifiedSbom {
            referrer_digest: digest_of(referrer.as_bytes()),
            subject_digest: subject.clone(),
            predicate_type: predicate_type.to_string(),
            document: CYCLONEDX_1_6.as_bytes().to_vec(),
        }
    }

    fn scan_over(
        platform_subject: Option<ocx_oci::Digest>,
        matches: Vec<AttestationMatch>,
        unverified: Vec<UnverifiedSbom>,
    ) -> AttestationScan {
        AttestationScan {
            matches,
            unverified,
            refused: Vec::new(),
            platform_subject,
        }
    }

    /// The three documents this fixture puts on one package, and the one assertion the
    /// whole contract exists for.
    fn three_documents_across_two_subjects() -> Vec<AttestationMatch> {
        vec![
            a_match_on(&platform_subject(), "platform-cyclonedx", CYCLONEDX),
            a_match_on(&enclosing_subject(), "index-cyclonedx", CYCLONEDX),
            a_match_on(&enclosing_subject(), "index-spdx", SPDX),
        ]
    }

    /// A platform-level CycloneDX supersedes the index-level
    /// CycloneDX and **must not touch** the index-level SPDX.
    ///
    /// Dropping the predicateType from the shadowing key is the whole difference
    /// between this contract and a data-loss bug: an SPDX document is not a
    /// substitute for a CycloneDX one, and a consumer that asked for SPDX would
    /// be told the package carries none.
    #[test]
    fn a_platform_document_shadows_only_its_own_predicate_type() {
        let report = report_from(scan_over(
            Some(platform_subject()),
            three_documents_across_two_subjects(),
            Vec::new(),
        ));

        assert!(
            report.shadowed.contains(&digest_of(b"index-cyclonedx")),
            "the index-level CycloneDX is superseded by the platform-level one of the same type",
        );
        assert!(
            !report.shadowed.contains(&digest_of(b"index-spdx")),
            "an index-level SPDX is a different document for a different consumer; a platform \
             CycloneDX hiding it is data loss, not a preference",
        );
        assert!(
            !report.shadowed.contains(&digest_of(b"platform-cyclonedx")),
            "the preferred document can never be its own shadow",
        );
        assert_eq!(
            report.attestations.len(),
            3,
            "shadowing marks; it never drops or reorders, so `--format json` still carries all three",
        );
    }

    /// Nothing was narrowed, so nothing is superseded — even
    /// with the identical document set.
    ///
    /// The discriminating control for the test above: the two differ **only** in
    /// `platform_subject`, so a shadowing pass that ignored it would fail here
    /// rather than pass both.
    #[test]
    fn nothing_is_shadowed_when_no_platform_was_selected() {
        let report = report_from(scan_over(None, three_documents_across_two_subjects(), Vec::new()));
        assert!(
            report.shadowed.is_empty(),
            "with no platform selected the listing reports all, grouped by subject: {:?}",
            report.shadowed,
        );
    }

    /// Two documents of one predicateType on the **same**
    /// subject shadow neither. Multiple SBOMs per package is normal — different
    /// formats, lifecycle phases, rescans — and there is no disambiguation
    /// convention beyond `org.opencontainers.image.created`, so the existing
    /// `MultipleAttestations` behaviour stands.
    #[test]
    fn two_documents_of_one_type_on_one_subject_shadow_neither() {
        let platform = report_from(scan_over(
            Some(platform_subject()),
            vec![
                a_match_on(&platform_subject(), "first-cyclonedx", CYCLONEDX),
                a_match_on(&platform_subject(), "second-cyclonedx", CYCLONEDX),
            ],
            Vec::new(),
        ));
        assert!(
            platform.shadowed.is_empty(),
            "two platform-level documents are two answers, not a preference: {:?}",
            platform.shadowed,
        );

        let enclosing = report_from(scan_over(
            Some(platform_subject()),
            vec![
                a_match_on(&enclosing_subject(), "first-cyclonedx", CYCLONEDX),
                a_match_on(&enclosing_subject(), "second-cyclonedx", CYCLONEDX),
            ],
            Vec::new(),
        ));
        assert!(
            enclosing.shadowed.is_empty(),
            "the platform manifest carries no CycloneDX, so neither index-level one is superseded: {:?}",
            enclosing.shadowed,
        );
    }

    /// The two trust classes share one vocabulary: a permissive listing shadows
    /// by predicateType exactly as a demanded one does. Keying only the verified
    /// half would leave `--no-verify` rendering both copies.
    #[test]
    fn shadowing_reads_one_vocabulary_across_both_trust_classes() {
        let report = report_from(scan_over(
            Some(platform_subject()),
            Vec::new(),
            vec![
                an_unverified_on(&platform_subject(), "platform-cyclonedx", CYCLONEDX),
                an_unverified_on(&enclosing_subject(), "index-cyclonedx", CYCLONEDX),
                an_unverified_on(&enclosing_subject(), "index-spdx", SPDX),
            ],
        ));
        assert!(report.shadowed.contains(&digest_of(b"index-cyclonedx")));
        assert!(
            !report.shadowed.contains(&digest_of(b"index-spdx")),
            "the predicateType scoping is the same rule on either trust class",
        );
    }

    /// The listing is ALL verified attestations, in listing order —
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
            platform_subject: None,
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
            platform_subject: None,
        };
        let report = report_from(scan);
        assert_eq!(report.attestations.len(), 1);
        assert!(report.refused.is_empty());
    }

    /// The predicate a match carries is the verbatim signed
    /// sub-slice, so it feeds `crate::tasks::sbom` directly. This is the seam the
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
            platform_subject: None,
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

    /// A scan that ends with no match — including the `--type`
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

        // Discriminating control: a different verify kind classifies
        // differently through the identical wrapper, so the assertion above
        // cannot be green for a wrapper that returns one fixed code.
        let other = super::super::verify::map_verify_error(
            tagged_id(),
            VerifyError::new(tagged_id(), VerifyErrorKind::MultipleSignatures { count: 2 }),
        );
        let _other_inner = match other.kind {
            PackageErrorKind::Internal(crate::Error::Verify(verify_error)) => verify_error,
            other => panic!("expected Internal(Verify(..)), got {other:?}"),
        };
    }

    /// `sbom_one` drives the attestation scan through the same pipeline as
    /// `verify_one` and wraps its failures identically — proven on the one
    /// pipeline outcome a unit test can reach without live Sigstore material.
    /// (The verified-listing paths are acceptance-tested: WP10a.)
    #[tokio::test(flavor = "multi_thread")]
    async fn sbom_one_wraps_no_signatures_found_and_never_grows_the_local_index() {
        let root = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(root.path().to_path_buf());
        let index_store = ocx_index::IndexStore::machine_local(&file_structure);
        let local_index = LocalIndex::new(LocalConfig {
            index_store: index_store.clone(),
        });
        let source = Index::from_impl(SingleTagSource);
        let index = Index::from_chained(local_index, vec![source], ChainMode::Default);
        let manager = PackageManager::new(file_structure, index, None, REGISTRY);

        let state = StateStore::new(root.path().join("state"));
        let client = ocx_oci::Client::with_transport(Box::new(transport_without_referrers()));
        let trust_root = TrustRoot::default();
        let rekor_url = Url::parse("http://127.0.0.1:3000").unwrap();
        let platform: ocx_oci::Platform = "linux/amd64".parse().unwrap();

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
            signature_format: None,
        };

        match manager.sbom_one(&tagged_id(), Some(&platform), opts).await {
            Err(err) => match err.kind {
                PackageErrorKind::Internal(crate::Error::Verify(verify_err)) => assert!(
                    matches!(verify_err.kind, VerifyErrorKind::NoSignaturesFound),
                    "expected NoSignaturesFound, got {:?}",
                    verify_err.kind
                ),
                other => panic!("expected Internal(Verify(NoSignaturesFound)), got {other:?}"),
            },
            Ok(_) => panic!("nothing is attested here; sbom_one must fail closed"),
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
