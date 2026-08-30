// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;

use ocx_lib::{cli::Cell, oci::LayerCounts, publisher::PushOutcome};
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::signature::SignatureReport;
use crate::api::data::sweep::SweptStatus;

/// Result of a successful `ocx package push`.
///
/// Plain format: a one-row table — `Identifier`, `Digest`, `Tags` (the rolling
/// cascade tags, comma-joined), `Keep Tags` (how many were written) and
/// `Layers` (the mounted/uploaded/verified counts). Keeps a plain push from
/// being silent; progress still surfaces on stderr via the log layer. `status`
/// is plain-omitted (it is the constant `"pushed"`) and the keep tags are
/// counted rather than listed — each is an 82-column
/// `__ocx.keep.sha256-<hex>` and there is one per distinct platform manifest.
///
/// JSON format:
/// `{ "identifier", "status", "manifest_digest", "cascade_tags_written",
/// "keep_tags_written", "layers": { "mounted", "uploaded", "verified" },
/// "platform_digests": { "<platform>": "sha256:…" } }`.
/// The first five keys are the machine-readable contract consumed by
/// `ocx-mirror pipeline push`, which keys its go/no-go bookkeeping off `status`
/// and records `cascade_tags_written` in the run summary; `layers` and
/// `platform_digests` are additive.
#[derive(Serialize)]
pub struct PushReport {
    /// The pushed package identifier (`registry/repository:tag`).
    pub identifier: String,
    /// Outcome of the push. Always `"pushed"`: the command performs the push
    /// unconditionally (the registry merge is idempotent).
    pub status: String,
    /// Digest of the pushed multi-platform image index (`sha256:...`).
    pub manifest_digest: String,
    /// Rolling cascade tags written in addition to the primary version tag
    /// (e.g. `3.28`, `3`, `latest`). Empty for a non-cascade push.
    pub cascade_tags_written: Vec<String>,
    /// Digest-named `__ocx.keep.<algorithm>-<hex>` tags this push wrote, in
    /// push order, one
    /// per distinct platform manifest — platforms whose manifest is identical
    /// share a single tag, so this does not zip against the pushed platform
    /// list. Empty under `--no-keep-tag`. Reports what reached the
    /// registry, not what was requested.
    pub keep_tags_written: Vec<String>,
    /// Counts of layer-push outcomes (mounted/uploaded/verified), summed over
    /// every platform this push fanned out to. Layer blobs only — the config
    /// blob and manifest are not layers.
    pub layers: LayerCounts,
    /// Platform manifest digests this push produced, keyed by the canonical
    /// platform string (`os/arch[/variant][+feature,…]`). The signing input
    /// for a later `push --sign`, and independent of `--keep-tag`: a
    /// `--no-keep-tag` push reports an empty `keep_tags_written` and this map
    /// fully populated.
    ///
    /// Distinct from `manifest_digest`, which names the tag's image index —
    /// rewritten on every platform merge, and therefore not what a signature
    /// can cover.
    ///
    /// A `BTreeMap` rather than an array: the access a consumer wants is
    /// `.platform_digests["linux/amd64"]`, and sorted output is
    /// deterministic. Two platforms sharing one manifest appear as two keys
    /// with one value. JSON only — the plain table is already at its
    /// five-column budget and a digest column would blow the width.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub platform_digests: BTreeMap<String, String>,
    /// One row per platform manifest `--sign` signed inline, in push order.
    ///
    /// Empty (and omitted) without `--sign`, so the key set a consumer of an
    /// unsigned push parses is unchanged. JSON only, exactly like
    /// `platform_digests` and `attestation`: the plain table is at its
    /// five-column budget, and a signing failure additionally reaches a plain
    /// caller on stderr.
    ///
    /// The index is deliberately absent from this list. `push` signs the
    /// platform manifests because their digests are final the moment they are
    /// pushed; the index digest is rewritten on every platform merge, so it is
    /// signed later by `ocx package sign --tags-file`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<SignedPlatformReport>,
    /// `None` unless `--sbom` was passed.
    ///
    /// Additive: `ocx-mirror pipeline push` keys its go/no-go off `status`, and
    /// `status` still reports the push alone. A push that lands and an
    /// attestation that then fails is a real state — the manifest is immutable
    /// and OCI offers no un-push — so the two outcomes are reported separately
    /// rather than folded into one verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestation: Option<AttestationOutcome>,
}

/// One platform manifest's inline-signing row.
///
/// The [`SweptTagReport`] shape with `tag` replaced by `platform`, and for the
/// same reasons: flat rather than an internally-tagged enum, because `report`
/// is a struct that a tagged enum could only carry through `flatten`, and a
/// `status` plus three optionals is the same information without that. It
/// reuses [`SweptStatus`] so a reader meets one vocabulary across `sign`'s
/// sweep and `push`'s inline signing, and [`SignatureReport`] verbatim so a
/// consumer parsing a `ocx package sign` document parses this one with the
/// same code, one level down.
///
/// `platform` rather than `tag` is the one honest difference: `push` signs the
/// platform manifests a push landed on, and putting `linux/amd64` in a field
/// every other report spells `tag` would name an OCI tag that does not exist.
/// `SweptStatus::Skipped` is unreachable here — a platform the merged index did
/// not carry is omitted from `platform_digests`, so it never becomes a row.
///
/// [`SweptTagReport`]: crate::api::data::sweep::SweptTagReport
#[derive(Serialize)]
pub struct SignedPlatformReport {
    /// The platform whose manifest was signed, canonically spelled
    /// (`os/arch[/variant][+feature,…]`) — the same key `platform_digests`
    /// uses, so the two zip.
    pub platform: String,
    /// What the inline signing did to this platform.
    pub status: SweptStatus,
    /// That platform's own sign report, verbatim. Present for every platform
    /// whose run produced one, `failed` rows included: a
    /// `--signature-format both` platform where one leg landed and one did not
    /// is a failure that still carries the leg that landed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<SignatureReport>,
    /// The JSON error envelope's per-variant slug for this platform's failure,
    /// falling back to its frozen category. Present exactly when `status` is
    /// `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable cause, sanitized for the terminal (CWE-150). Present
    /// exactly when `status` is `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl SignedPlatformReport {
    /// A platform whose manifest was signed.
    pub fn completed(platform: String, report: SignatureReport) -> Self {
        Self {
            platform,
            status: SweptStatus::Completed,
            report: Some(report),
            kind: None,
            message: None,
        }
    }

    /// A platform whose signing failed, described the way the error envelope
    /// would describe it.
    pub fn failed(platform: String, report: Option<SignatureReport>, kind: String, message: String) -> Self {
        Self {
            platform,
            status: SweptStatus::Failed,
            report,
            kind: Some(kind),
            message: Some(crate::api::data::sanitize_for_terminal(&message)),
        }
    }
}

/// What `--sbom` did after the push landed.
///
/// A failure carries the error slug the JSON error envelope would have used
/// (CLI-04), not a bespoke string, so a script branches on the same vocabulary
/// either way.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AttestationOutcome {
    /// The attestation was published on the pushed manifest.
    Succeeded {
        /// Digest of the published OCI referrer manifest. Absent under
        /// `--signature-format simplesigning`, which publishes the
        /// `sha256-<hex>.att` sidecar alone and no referrer.
        #[serde(skip_serializing_if = "Option::is_none")]
        referrer_digest: Option<String>,
        /// Digest of the `sha256-<hex>.att` sidecar manifest, when
        /// `--signature-format` asked for one. The spelling is
        /// [`AttestationReport`](crate::api::data::attestation::AttestationReport)'s,
        /// so one vocabulary describes the same two addresses in both reports.
        #[serde(skip_serializing_if = "Option::is_none")]
        sidecar_digest: Option<String>,
        /// The resolved `predicateType` URI written into the Statement.
        predicate_type: String,
        /// Whether the referrer carries a signature. `false` means the SBOM
        /// was attached raw because the run had no signing identity available
        /// — the push still succeeded, and nothing vouches for the document.
        signed: bool,
    },
    /// The push landed and was NOT rolled back; the attestation did not.
    Failed {
        /// The error envelope's per-variant slug (`error.detail`), falling
        /// back to its frozen category (`error.kind`) for errors outside the
        /// sign and verify taxonomies, which carry no `detail`.
        kind: String,
        /// Human-readable cause, sanitized for the terminal.
        message: String,
    },
}

impl PushReport {
    /// Builds a `pushed` report for `identifier` from the publisher's outcome.
    ///
    /// Takes the whole [`PushOutcome`] rather than its fields: the cascade and
    /// keep tags are both `Vec<String>`, so as adjacent positionals a
    /// swapped pair would type-check silently and publish
    /// `__ocx.keep.sha256-<hex>` values under `cascade_tags_written`.
    pub fn from_outcome(identifier: String, outcome: PushOutcome) -> Self {
        Self {
            identifier,
            status: "pushed".to_string(),
            manifest_digest: outcome.manifest_digest.to_string(),
            cascade_tags_written: outcome.cascade_tags,
            keep_tags_written: outcome.keep_tags,
            layers: outcome.layer_counts,
            platform_digests: outcome
                .platform_digests
                .into_iter()
                .map(|(platform, digest)| (platform.to_string(), digest.to_string()))
                .collect(),
            signatures: Vec::new(),
            attestation: None,
        }
    }

    /// Attach the inline-signing rows to a report already built from the push.
    ///
    /// Separate from [`Self::from_outcome`] for the same reason
    /// [`Self::with_attestation`] is: the push is not undoable, so its result
    /// is owed to the caller whatever the signing does next.
    #[must_use]
    pub fn with_signatures(mut self, signatures: Vec<SignedPlatformReport>) -> Self {
        self.signatures = signatures;
        self
    }

    /// Attach the `--sbom` outcome to a report already built from the push.
    ///
    /// Separate from [`Self::from_outcome`] because the push report must be
    /// constructible before the attestation is attempted: the push is not
    /// undoable, so its result is owed to the caller whatever happens next.
    #[must_use]
    pub fn with_attestation(mut self, attestation: AttestationOutcome) -> Self {
        self.attestation = Some(attestation);
        self
    }
}

impl Printable for PushReport {
    /// One-row table: identifier, digest, the rolling cascade tags, the number
    /// of keep tags written, and the layer-push counter breakdown. Machine
    /// consumers should prefer `--format json`; this line keeps a plain push
    /// from emitting nothing.
    ///
    /// `status` has no column because it is always `"pushed"`, and the
    /// keep tags are a count because listing them is 82 columns each.
    /// Both stay in the JSON contract, where `ocx-mirror` reads them.
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        data.print_table(
            &[
                "Identifier".into(),
                "Digest".into(),
                "Tags".into(),
                "Keep Tags".into(),
                "Layers".into(),
            ],
            &[
                vec![Cell::from(self.identifier.clone())],
                vec![Cell::from(self.manifest_digest.clone())],
                vec![Cell::from(self.cascade_tags_written.join(","))],
                vec![Cell::from(self.keep_tags_written.len().to_string())],
                vec![Cell::from(format!(
                    "mounted={},uploaded={},verified={}",
                    self.layers.mounted, self.layers.uploaded, self.layers.verified
                ))],
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::{
        cli::{DataInterface, Printer},
        oci::{self, LayerCounts},
        publisher::PushOutcome,
    };

    use super::PushReport;
    use crate::api::Printable as _;

    /// A `sha256:` digest whose hex is one repeated character, so a test can
    /// name distinct digests by a single letter and still read them back.
    fn digest(hex_char: &str) -> oci::Digest {
        oci::Digest::try_from(format!("sha256:{}", hex_char.repeat(64)).as_str()).expect("digest parses")
    }

    fn outcome(digest_hex: &str, cascade_tags: Vec<String>, keep_tags: Vec<String>) -> PushOutcome {
        outcome_with_layers(digest_hex, cascade_tags, keep_tags, LayerCounts::default())
    }

    /// `PushOutcome` is `#[non_exhaustive]`, so this crate can reach it through
    /// neither a struct literal nor functional-update syntax.
    fn outcome_with_layers(
        digest_hex: &str,
        cascade_tags: Vec<String>,
        keep_tags: Vec<String>,
        layer_counts: LayerCounts,
    ) -> PushOutcome {
        PushOutcome::new(digest(digest_hex), cascade_tags, keep_tags, Vec::new(), layer_counts)
    }

    /// A two-platform push: the index digest and each platform's manifest
    /// digest are three distinct values, which is what makes the assertions
    /// below able to tell them apart.
    fn multi_platform_outcome(keep_tags: Vec<String>) -> PushOutcome {
        PushOutcome::new(
            digest("e"),
            Vec::new(),
            keep_tags,
            vec![
                ("linux/amd64".parse().expect("platform parses"), digest("1")),
                ("linux/arm64/v8".parse().expect("platform parses"), digest("2")),
            ],
            LayerCounts::default(),
        )
    }

    /// Pins the JSON wire format consumed by `ocx-mirror pipeline push`: the
    /// five keys (`identifier`, `status`, `manifest_digest`,
    /// `cascade_tags_written`, `keep_tags_written`) and the constant
    /// `"pushed"` status. The mirror parser keys its go/no-go bookkeeping off
    /// these names.
    #[test]
    fn cascade_report_json_shape() {
        let report = PushReport::from_outcome(
            "registry.example/tool:3.28.1".to_string(),
            outcome(
                "c",
                vec!["3.28".to_string(), "3".to_string(), "latest".to_string()],
                vec![format!("__ocx.keep.sha256-{}", "a".repeat(64))],
            ),
        );
        let value = serde_json::to_value(&report).unwrap();

        assert_eq!(
            value.get("identifier").and_then(|v| v.as_str()),
            Some("registry.example/tool:3.28.1")
        );
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("pushed"));
        assert_eq!(
            value.get("manifest_digest").and_then(|v| v.as_str()),
            Some(format!("sha256:{}", "c".repeat(64)).as_str())
        );
        assert_eq!(
            value.get("cascade_tags_written").and_then(|v| v.as_array()),
            Some(&vec!["3.28".into(), "3".into(), "latest".into()])
        );
        assert_eq!(
            value.get("keep_tags_written").and_then(|v| v.as_array()),
            Some(&vec![format!("__ocx.keep.sha256-{}", "a".repeat(64)).into()])
        );
    }

    /// A non-cascade push with `--no-keep-tag` writes neither tag family:
    /// both arrays must serialize as empty, never absent or null.
    #[test]
    fn non_cascade_report_has_empty_tags() {
        let report = PushReport::from_outcome("tool:1.0.0".to_string(), outcome("d", Vec::new(), Vec::new()));
        let value = serde_json::to_value(&report).unwrap();

        assert_eq!(
            value.get("cascade_tags_written").and_then(|v| v.as_array()),
            Some(&Vec::new())
        );
        assert_eq!(
            value.get("keep_tags_written").and_then(|v| v.as_array()),
            Some(&Vec::new())
        );
    }

    /// `layers` serializes as an additive keyed object with the three
    /// mount/upload/verify counts.
    #[test]
    fn layers_json_shape() {
        let report = PushReport::from_outcome(
            "tool:1.0.0".to_string(),
            outcome_with_layers(
                "d",
                Vec::new(),
                Vec::new(),
                LayerCounts {
                    mounted: 2,
                    uploaded: 1,
                    verified: 3,
                },
            ),
        );
        let value = serde_json::to_value(&report).unwrap();

        let layers = value.get("layers").expect("layers key present");
        assert_eq!(layers.get("mounted").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(layers.get("uploaded").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(layers.get("verified").and_then(|v| v.as_u64()), Some(3));
    }

    /// T-18. `platform_digests` carries the **platform manifest** digest for
    /// every pushed platform, keyed by the canonical platform string — never
    /// the index digest, which `manifest_digest` already reports and which is
    /// rewritten by the next platform merge.
    ///
    /// The fixture is deliberately multi-platform with three distinct digests:
    /// a single-platform fixture cannot tell "the platform manifest" from
    /// "the index", so this assertion would pass on the wrong value.
    #[test]
    fn push_report_json_shape_carries_per_platform_manifest_digests() {
        let report = PushReport::from_outcome("tool:1.0.0".to_string(), multi_platform_outcome(Vec::new()));
        let value = serde_json::to_value(&report).expect("serialize");

        let digests = value
            .get("platform_digests")
            .and_then(|v| v.as_object())
            .expect("platform_digests key present");
        assert_eq!(digests.len(), 2, "one entry per pushed platform: {digests:?}");
        assert_eq!(
            digests.get("linux/amd64").and_then(|v| v.as_str()),
            Some(format!("sha256:{}", "1".repeat(64)).as_str())
        );
        assert_eq!(
            digests.get("linux/arm64/v8").and_then(|v| v.as_str()),
            Some(format!("sha256:{}", "2".repeat(64)).as_str())
        );
        let index_digest = value
            .get("manifest_digest")
            .and_then(|v| v.as_str())
            .expect("manifest_digest present");
        for (platform, digest) in digests {
            assert_ne!(
                digest.as_str(),
                Some(index_digest),
                "{platform} must report its own manifest digest, not the index digest"
            );
        }
    }

    /// A push that produced no platform manifest omits the key rather than
    /// emitting `{}` — an empty object would claim the push looked and found
    /// nothing, which is a different statement.
    #[test]
    fn push_report_omits_platform_digests_when_none_were_produced() {
        let report = PushReport::from_outcome("tool:1.0.0".to_string(), outcome("d", Vec::new(), Vec::new()));
        let value = serde_json::to_value(&report).expect("serialize");

        assert!(
            value.get("platform_digests").is_none(),
            "platform_digests must be omitted, not an empty object: {value}"
        );
    }

    /// `platform_digests` is independent of keep tagging. A `--no-keep-tag`
    /// push writes no keep tag and must still report every platform digest —
    /// deriving the field from `keep_tags_written` would empty it here.
    #[test]
    fn platform_digests_survive_no_keep_tag() {
        let report = PushReport::from_outcome("tool:1.0.0".to_string(), multi_platform_outcome(Vec::new()));
        let value = serde_json::to_value(&report).expect("serialize");

        assert_eq!(
            value.get("keep_tags_written").and_then(|v| v.as_array()),
            Some(&Vec::new()),
            "the fixture is a --no-keep-tag push"
        );
        assert_eq!(
            value
                .get("platform_digests")
                .and_then(|v| v.as_object())
                .map(serde_json::Map::len),
            Some(2),
            "keep tagging off must not cost the platform digests: {value}"
        );
    }

    /// `print_plain` emits the one-row table without panicking when colour is
    /// disabled — keeps a plain push from being silent.
    #[test]
    fn print_plain_smoke() {
        let report = PushReport::from_outcome(
            "tool:1.0.0".to_string(),
            outcome_with_layers(
                "d",
                vec!["1".to_string(), "latest".to_string()],
                vec![format!("__ocx.keep.sha256-{}", "b".repeat(64))],
                LayerCounts {
                    mounted: 1,
                    uploaded: 0,
                    verified: 0,
                },
            ),
        );
        let data = DataInterface::new(Printer::new(false, false));
        report.print_plain(&data);
    }
}

#[cfg(test)]
mod attestation_tests {
    use super::{AttestationOutcome, PushReport};

    fn report() -> PushReport {
        PushReport {
            identifier: "registry.example/pkg:1.0".into(),
            status: "pushed".into(),
            manifest_digest: format!("sha256:{}", "a".repeat(64)),
            cascade_tags_written: Vec::new(),
            keep_tags_written: Vec::new(),
            layers: ocx_lib::oci::LayerCounts::default(),
            platform_digests: std::collections::BTreeMap::new(),
            signatures: Vec::new(),
            attestation: None,
        }
    }

    /// A push without `--sbom` emits exactly the keys `ocx-mirror pipeline
    /// push` already parses. The field is additive, so it must be absent
    /// rather than `null`.
    #[test]
    fn a_push_without_sbom_omits_the_attestation_key() {
        let json = serde_json::to_value(report()).expect("serialize");
        assert!(
            json.get("attestation").is_none(),
            "attestation must be omitted, not null: {json}"
        );
        assert_eq!(json["status"], "pushed");
    }

    /// `status` still reports the push alone: a landed push with a failed
    /// attestation is a real state, and folding the two into one verdict would
    /// tell a mirror pipeline the push did not happen.
    #[test]
    fn a_failed_attestation_does_not_change_the_push_status() {
        let json = serde_json::to_value(report().with_attestation(AttestationOutcome::Failed {
            kind: "offline_attest_refused".into(),
            message: "offline attestation is not supported".into(),
        }))
        .expect("serialize");

        assert_eq!(json["status"], "pushed", "the push landed and is not undoable");
        assert_eq!(json["attestation"]["status"], "failed");
        assert_eq!(json["attestation"]["kind"], "offline_attest_refused");
    }

    #[test]
    fn a_successful_attestation_reports_the_referrer_and_resolved_type() {
        let json = serde_json::to_value(report().with_attestation(AttestationOutcome::Succeeded {
            referrer_digest: Some(format!("sha256:{}", "c".repeat(64))),
            sidecar_digest: None,
            predicate_type: "https://cyclonedx.org/bom".into(),
            signed: true,
        }))
        .expect("serialize");

        assert_eq!(json["attestation"]["status"], "succeeded");
        assert_eq!(
            json["attestation"]["referrer_digest"],
            format!("sha256:{}", "c".repeat(64))
        );
        assert_eq!(json["attestation"]["predicate_type"], "https://cyclonedx.org/bom");
        assert_eq!(json["attestation"]["signed"], true);
        assert!(
            json["attestation"].get("sidecar_digest").is_none(),
            "a bundle-only attach must omit the sidecar key, not emit it null: {json}"
        );
    }

    /// `--signature-format simplesigning` writes the `sha256-<hex>.att`
    /// sidecar and no referrer. That is a published attestation, so the row
    /// says `succeeded` and names the address that was actually written —
    /// before this pair, `push` reported the same run as `failed` and folded a
    /// non-zero code into its exit status.
    ///
    /// Asserted together with its bundle twin above so neither shape can pass
    /// by the other key having been dropped for everyone.
    #[test]
    fn a_sidecar_only_attestation_reports_the_sidecar_and_omits_the_referrer() {
        let json = serde_json::to_value(report().with_attestation(AttestationOutcome::Succeeded {
            referrer_digest: None,
            sidecar_digest: Some(format!("sha256:{}", "d".repeat(64))),
            predicate_type: "https://cyclonedx.org/bom".into(),
            signed: true,
        }))
        .expect("serialize");

        assert_eq!(json["attestation"]["status"], "succeeded");
        assert_eq!(
            json["attestation"]["sidecar_digest"],
            format!("sha256:{}", "d".repeat(64))
        );
        assert!(
            json["attestation"].get("referrer_digest").is_none(),
            "a sidecar-only attach must omit the referrer key: {json}"
        );
    }

    /// A push that attached an SBOM with no identity behind it still reports
    /// `succeeded`, and says so in the one field that distinguishes the two.
    /// Without it a CI job whose OIDC configuration silently broke would read
    /// as having published a signed SBOM.
    #[test]
    fn an_unsigned_attachment_succeeds_and_says_it_is_unsigned() {
        let json = serde_json::to_value(report().with_attestation(AttestationOutcome::Succeeded {
            referrer_digest: Some(format!("sha256:{}", "c".repeat(64))),
            sidecar_digest: None,
            predicate_type: "https://cyclonedx.org/bom".into(),
            signed: false,
        }))
        .expect("serialize");

        assert_eq!(json["attestation"]["status"], "succeeded");
        assert_eq!(json["attestation"]["signed"], false);
    }
}

#[cfg(test)]
mod signature_row_tests {
    //! The `signatures` array: additive, keyed by platform, and reusing
    //! `SignatureReport` verbatim so a consumer parsing an `ocx package sign`
    //! document parses one of these rows with the same code, one level down.

    use ocx_lib::oci;

    use super::{PushReport, SignedPlatformReport};
    use crate::api::data::signature::{SignatureLegReport, SignatureReport};

    fn digest(hex_char: &str) -> oci::Digest {
        oci::Digest::try_from(format!("sha256:{}", hex_char.repeat(64)).as_str()).expect("digest parses")
    }

    fn report() -> PushReport {
        PushReport {
            identifier: "registry.example/pkg:1.0".into(),
            status: "pushed".into(),
            manifest_digest: format!("sha256:{}", "a".repeat(64)),
            cascade_tags_written: Vec::new(),
            keep_tags_written: Vec::new(),
            layers: ocx_lib::oci::LayerCounts::default(),
            platform_digests: std::collections::BTreeMap::new(),
            signatures: Vec::new(),
            attestation: None,
        }
    }

    fn signature(subject: oci::Digest) -> SignatureReport {
        SignatureReport::new(
            "registry.example/pkg:1.0".into(),
            subject,
            vec![SignatureLegReport {
                format: oci::sign::SignatureFormat::Bundle,
                payload_digest: Some(digest("b")),
                manifest_digest: Some(digest("c")),
                error: None,
            }],
            Some(&"linux/amd64".parse::<oci::Platform>().expect("platform parses")),
            String::new(),
            String::new(),
        )
    }

    /// A push without `--sign` carries the key set `ocx-mirror pipeline push`
    /// already parses. Additive means absent, never an empty array.
    #[test]
    fn a_push_without_sign_omits_the_signatures_key() {
        let json = serde_json::to_value(report()).expect("serialize");
        assert!(
            json.get("signatures").is_none(),
            "signatures must be omitted, not empty: {json}"
        );
    }

    /// The signed subject is the platform manifest's digest, never the index's
    /// — the whole reason `push` signs inline and `sign --tags-file` sweeps the
    /// index later.
    #[test]
    fn a_signed_row_names_the_platform_and_carries_that_platforms_subject_digest() {
        let push = report().with_signatures(vec![SignedPlatformReport::completed(
            "linux/amd64".into(),
            signature(digest("1")),
        )]);
        let json = serde_json::to_value(push).expect("serialize");

        assert_eq!(json["signatures"][0]["platform"], "linux/amd64");
        assert_eq!(json["signatures"][0]["status"], "completed");
        assert_eq!(
            json["signatures"][0]["report"]["subject_digest"],
            format!("sha256:{}", "1".repeat(64)),
            "the subject must be the platform manifest, not the index digest {}",
            json["manifest_digest"]
        );
        assert_ne!(
            json["signatures"][0]["report"]["subject_digest"], json["manifest_digest"],
            "signing the index inline would leave a dead signature on every later merge"
        );
        assert!(
            json["signatures"][0].get("kind").is_none(),
            "a completed row carries no failure slug"
        );
    }

    /// A landed push with a failed signature is a real state: `status` still
    /// reports the push alone, and the row carries the envelope's own slug.
    #[test]
    fn a_failed_signature_does_not_change_the_push_status() {
        let push = report().with_signatures(vec![SignedPlatformReport::failed(
            "linux/arm64".into(),
            None,
            "referrers_unsupported".into(),
            "the registry serves no referrers API".into(),
        )]);
        let json = serde_json::to_value(push).expect("serialize");

        assert_eq!(json["status"], "pushed", "the push landed and is not undoable");
        assert_eq!(json["signatures"][0]["status"], "failed");
        assert_eq!(json["signatures"][0]["kind"], "referrers_unsupported");
        assert_eq!(json["signatures"][0]["message"], "the registry serves no referrers API");
        assert!(
            json["signatures"][0].get("report").is_none(),
            "a run that produced no report carries none"
        );
    }

    /// A `--signature-format both` platform that lost one leg is `failed` and
    /// still carries the leg that landed — hiding it would leave the operator
    /// re-signing what is already published.
    #[test]
    fn a_partially_failed_platform_keeps_the_leg_that_landed() {
        let push = report().with_signatures(vec![SignedPlatformReport::failed(
            "linux/amd64".into(),
            Some(signature(digest("1"))),
            "internal".into(),
            "the simplesigning sidecar was refused".into(),
        )]);
        let json = serde_json::to_value(push).expect("serialize");

        assert_eq!(json["signatures"][0]["status"], "failed");
        assert_eq!(json["signatures"][0]["report"]["legs"][0]["format"], "bundle");
    }

    /// Registry-sourced text reaches an error chain verbatim (CWE-150) and this
    /// string is rendered to a terminal.
    #[test]
    fn a_failure_message_is_sanitized_for_the_terminal() {
        let push = report().with_signatures(vec![SignedPlatformReport::failed(
            "linux/amd64".into(),
            None,
            "internal".into(),
            "boom\u{1b}[31m".into(),
        )]);
        let json = serde_json::to_value(push).expect("serialize");
        let message = json["signatures"][0]["message"].as_str().expect("a message");
        assert!(!message.contains('\u{1b}'), "escape sequence survived: {message:?}");
    }
}
