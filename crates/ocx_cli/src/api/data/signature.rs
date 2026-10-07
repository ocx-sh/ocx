// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package sign` output.

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Summary of a signing operation, keyless or under a key.
///
/// Digests are always full in JSON. A leg's two digests are not
/// interchangeable: `payload_digest` is the SHA-256 of the signed blob (what
/// the transparency record covers), `manifest_digest` that of the manifest it
/// hangs from. `signer` is `"keyless-fulcio"`, or the key backend's own slug
/// under a key. A partial run (one leg failed) still prints this report and
/// exits non-zero.
#[derive(Serialize, schemars::JsonSchema)]
pub struct SignatureReport {
    /// The package that was signed, resolved against the default registry.
    pub identifier: ocx_oci::PackageRef,
    /// Digest of the subject manifest that the bundle signs.
    pub subject_digest: ocx_oci::Digest,
    /// One entry per wire shape that was written or attempted, in write order.
    ///
    /// `--signature-format both` emits two **independent** signatures, so the
    /// run is best-effort per leg rather than atomic: a leg that failed is
    /// reported alongside one that succeeded, and the exit code comes from the
    /// failure.
    pub legs: Vec<SignatureLegReport>,
    /// The `--platform` the run narrowed into; absent when none was given and
    /// the run signed whatever the identifier resolved to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<ocx_oci::Platform>,
    /// Signing mechanism used: `"keyless-fulcio"`, or the key backend's own slug under a key.
    pub signer: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert. Absent under a
    /// key, and when no leg was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert. Absent under a
    /// key, and when no leg was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Which key model produced this signature: `keyless`, `file`, or a
    /// key-backend scheme.
    pub key_backend: ocx_trust::key_ref::KeyBackendKind,
    /// The signing key's cosign hint, in key mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_hint: Option<String>,
    /// The Rekor log index of the transparency record this run created.
    ///
    /// Absent when no record was created: legal under a key, where
    /// `--rekor-upload` is opt-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparency_log_index: Option<u64>,
}

/// One wire shape's outcome, as reported.
#[derive(Serialize, schemars::JsonSchema)]
pub struct SignatureLegReport {
    /// The shape: `bundle` or `simplesigning`.
    pub format: ocx_sign::sign::SignatureFormat,
    /// Digest of the signed payload blob — the Sigstore bundle under `bundle`,
    /// the simplesigning claim under `simplesigning`. Absent when the leg failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_digest: Option<ocx_oci::Digest>,
    /// Digest of the manifest the payload hangs from — the OCI referrer under
    /// `bundle`, the `sha256-<hex>.sig` sidecar under `simplesigning`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<ocx_oci::Digest>,
    /// Why the leg failed, when it did. `None` means it was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The signing pipeline's empty certificate field (key mode, or no leg written) as absent.
fn certificate_field(value: String) -> Option<String> {
    Some(value).filter(|value| !value.is_empty())
}

impl SignatureReport {
    pub fn new(
        identifier: ocx_oci::PackageRef,
        subject_digest: ocx_oci::Digest,
        legs: Vec<SignatureLegReport>,
        platform: Option<&ocx_oci::Platform>,
        certificate_identity: String,
        certificate_oidc_issuer: String,
    ) -> Self {
        Self {
            identifier,
            subject_digest,
            legs,
            platform: platform.cloned(),
            signer: "keyless-fulcio".to_string(),
            certificate_identity: certificate_field(certificate_identity),
            certificate_oidc_issuer: certificate_field(certificate_oidc_issuer),
            key_backend: ocx_trust::key_ref::KeyBackendKind::Keyless,
            public_key_hint: None,
            transparency_log_index: None,
        }
    }

    /// Record which key model signed (moving `signer` with it), and its key hint under a key.
    #[must_use]
    pub fn with_key_model(mut self, backend: ocx_trust::key_ref::KeyBackendKind, hint: Option<String>) -> Self {
        self.signer = match backend {
            ocx_trust::key_ref::KeyBackendKind::Keyless => "keyless-fulcio".to_string(),
            other => other.to_string(),
        };
        self.key_backend = backend;
        self.public_key_hint = hint;
        self
    }

    /// Record whether a transparency record was created.
    #[must_use]
    pub fn with_transparency_log(mut self, log_index: Option<u64>) -> Self {
        self.transparency_log_index = log_index;
        self
    }
}

impl SignatureReport {
    /// The (label, value) rows `print_plain` renders; only `subject_digest` stays a full digest.
    ///
    /// Split out because `Printer` has no injectable writer, so tests pin the rows here.
    /// Every value is sanitized (CWE-150), typed ones too: certificate fields and argv are
    /// foreign input, and a per-field filter would need re-arguing for each new field.
    fn plain_fields(&self) -> Vec<(String, String)> {
        let mut fields = vec![
            (
                "Identifier".to_string(),
                sanitize_for_terminal(&self.identifier.to_string()),
            ),
            (
                "Subject digest".to_string(),
                sanitize_for_terminal(&self.subject_digest.to_string()),
            ),
            (
                "Platform".to_string(),
                sanitize_for_terminal(
                    &self
                        .platform
                        .as_ref()
                        .map_or_else(|| "any".to_string(), ToString::to_string),
                ),
            ),
            (
                "Certificate identity".to_string(),
                sanitize_for_terminal(self.certificate_identity.as_deref().unwrap_or_default()),
            ),
            (
                "Certificate OIDC issuer".to_string(),
                sanitize_for_terminal(self.certificate_oidc_issuer.as_deref().unwrap_or_default()),
            ),
            (
                "Key backend".to_string(),
                sanitize_for_terminal(&self.key_backend.to_string()),
            ),
            // Stated as `none`, never omitted: no record is a legal outcome under a key.
            (
                "Transparency log".to_string(),
                match self.transparency_log_index {
                    Some(index) => format!("Rekor index {index}"),
                    None => "none".to_string(),
                },
            ),
        ];
        for leg in &self.legs {
            let value = match (&leg.manifest_digest, &leg.error) {
                (Some(digest), _) => digest.to_short_string(),
                (None, Some(error)) => format!("failed: {error}"),
                (None, None) => "unknown".to_string(),
            };
            fields.push((format!("Signature ({})", leg.format), sanitize_for_terminal(&value)));
        }
        fields
    }
}

impl crate::api::data::sweep::SweptReport for SignatureReport {
    const SWEEP_ROOT: &'static str = "SweepReport<SignatureReport>";
}

impl Printable for SignatureReport {
    const SCHEMA_VERSION: u32 = 2;
    const ROOT: &'static str = "SignatureReport";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let mut rows: [Vec<Cell>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in self.plain_fields() {
            rows[0].push(Cell::from(label));
            rows[1].push(Cell::from(value));
        }
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::data::is_bidi_control;

    fn id() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::parse_with_default_registry("registry.example/pkg:1.0", "ocx.sh").expect("identifier")
    }

    /// One written `bundle` leg — the default `--signature-format`.
    fn bundle_leg() -> SignatureLegReport {
        SignatureLegReport {
            format: ocx_sign::sign::SignatureFormat::Bundle,
            payload_digest: Some(ocx_oci::Digest::Sha256("b".repeat(64))),
            manifest_digest: Some(ocx_oci::Digest::Sha256("c".repeat(64))),
            error: None,
        }
    }

    fn sample_report() -> SignatureReport {
        SignatureReport::new(
            id(),
            ocx_oci::Digest::Sha256("a".repeat(64)),
            vec![bundle_leg()],
            Some(&"linux/amd64".parse().expect("platform")),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
        )
    }

    /// A run where the simplesigning leg failed and the bundle leg landed.
    fn partially_failed_report() -> SignatureReport {
        SignatureReport::new(
            id(),
            ocx_oci::Digest::Sha256("a".repeat(64)),
            vec![
                bundle_leg(),
                SignatureLegReport {
                    format: ocx_sign::sign::SignatureFormat::Simplesigning,
                    payload_digest: None,
                    manifest_digest: None,
                    error: Some("transient registry failure".into()),
                },
            ],
            Some(&"linux/amd64".parse().expect("platform")),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
        )
    }

    /// A partial `--signature-format both` run still reports the leg that
    /// landed: hiding it would leave the operator re-signing what is already
    /// published. The exit code is the process's alone; the report carries none.
    #[test]
    fn a_partial_run_reports_the_landed_leg_and_no_exit_code() {
        let document = serde_json::to_value(partially_failed_report()).expect("serialize");
        assert_eq!(
            document["legs"][0]["manifest_digest"],
            format!("sha256:{}", "c".repeat(64))
        );
        assert!(document["legs"][1]["error"].is_string());
        assert!(document.get("exit_code").is_none(), "{document}");
    }

    #[test]
    fn json_output_is_the_unwrapped_report() {
        let document = serde_json::to_value(sample_report()).expect("serialize");
        for envelope_key in ["schema_version", "command", "exit_code", "data"] {
            assert!(
                document.get(envelope_key).is_none(),
                "{envelope_key} leaked: {document}"
            );
        }
        assert_eq!(document["identifier"], "registry.example/pkg:1.0");
        // JSON keeps every digest full, unlike plain mode — a shortened 12-hex
        // form also satisfies `starts_with("sha256:")`, so exact equality is
        // required to pin that JSON never shortens (see
        // `print_plain_shortens_the_leg_digests_but_not_the_subject`
        // for the plain-mode counterpart).
        assert_eq!(document["subject_digest"], format!("sha256:{}", "a".repeat(64)));
        assert_eq!(document["legs"][0]["format"], "bundle");
        assert_eq!(
            document["legs"][0]["payload_digest"],
            format!("sha256:{}", "b".repeat(64))
        );
        assert_eq!(
            document["legs"][0]["manifest_digest"],
            format!("sha256:{}", "c".repeat(64))
        );
        assert_eq!(
            document["platform"],
            serde_json::json!({"architecture": "amd64", "os": "linux"}),
            "platform is the OCI platform object"
        );
        assert_eq!(document["signer"], "keyless-fulcio");
    }

    /// Absent, never `null` or empty: no `--platform`, no transparency record,
    /// and a key's missing certificate are each stated by the key's absence.
    #[test]
    fn unset_facts_are_absent_rather_than_null_or_empty() {
        let report = SignatureReport::new(
            id(),
            ocx_oci::Digest::Sha256("a".repeat(64)),
            vec![bundle_leg()],
            None,
            String::new(),
            String::new(),
        )
        .with_key_model(ocx_trust::key_ref::KeyBackendKind::File, Some("hint".into()));
        let document = serde_json::to_value(&report).expect("serialize");
        for absent in [
            "platform",
            "certificate_identity",
            "certificate_oidc_issuer",
            "transparency_log_index",
        ] {
            assert!(document.get(absent).is_none(), "{absent} must be absent: {document}");
        }
        // Positive control: the same keys are present once the facts exist.
        let logged = serde_json::to_value(sample_report().with_transparency_log(Some(7))).expect("serialize");
        for present in [
            "platform",
            "certificate_identity",
            "certificate_oidc_issuer",
            "transparency_log_index",
        ] {
            assert!(logged.get(present).is_some(), "{present} must be present: {logged}");
        }
    }

    /// `print_plain` shortens `bundle_digest`/`referrer_digest` to 12 hex (only
    /// `subject_digest` earns a full `sha256:<64hex>` row) and drops `signer`
    /// (constant for Slice 1) — smoke-checks the table renders without panic.
    #[test]
    fn print_plain_smoke() {
        let report = sample_report();
        let data = ocx_console::DataInterface::new(ocx_console::Printer::new(false, false));
        report.print_plain(&data);
    }

    /// Pins the plain-mode digest-shortening contract on the actual
    /// `(label, value)` pairs `print_plain` renders: `subject_digest` stays
    /// full (it is the answer), `bundle_digest`/`referrer_digest` shorten to
    /// 12 hex, and `signer` has no row at all (constant for Slice 1, present
    /// only in JSON).
    #[test]
    fn print_plain_shortens_the_leg_digests_but_not_the_subject() {
        let report = sample_report();
        let fields = report.plain_fields();
        let value_for = |label: &str| -> String {
            fields
                .iter()
                .find(|(field_label, _)| field_label == label)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("missing field {label:?} in plain_fields()"))
        };

        assert_eq!(
            value_for("Subject digest"),
            format!("sha256:{}", "a".repeat(64)),
            "subject digest must stay full — it is the answer"
        );
        let leg = value_for("Signature (bundle)");
        assert_eq!(
            leg,
            ocx_oci::Digest::Sha256("c".repeat(64)).to_short_string(),
            "a leg's manifest digest shortens to 12 hex"
        );
        assert_ne!(
            leg,
            format!("sha256:{}", "c".repeat(64)),
            "a leg digest must not render the full 64-hex form"
        );
        assert!(
            fields.iter().all(|(label, _)| label != "Signer"),
            "signer has no row in plain mode; found row: {fields:?}"
        );
    }

    /// `--signature-format both` renders **two** rows, and a leg that failed
    /// says so rather than vanishing — the property that stops a partial run
    /// reading as a clean one.
    #[test]
    fn both_legs_get_a_row_and_a_failed_leg_says_so() {
        let report = SignatureReport::new(
            id(),
            ocx_oci::Digest::Sha256("a".repeat(64)),
            vec![
                bundle_leg(),
                SignatureLegReport {
                    format: ocx_sign::sign::SignatureFormat::Simplesigning,
                    payload_digest: None,
                    manifest_digest: None,
                    error: Some("registry said no".to_string()),
                },
            ],
            Some(&"linux/amd64".parse().expect("platform")),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
        );
        let fields = report.plain_fields();
        let labels: Vec<&str> = fields.iter().map(|(label, _)| label.as_str()).collect();
        assert!(
            labels.contains(&"Signature (bundle)") && labels.contains(&"Signature (simplesigning)"),
            "each selected format earns its own row; got {labels:?}"
        );
        let failed = fields
            .iter()
            .find(|(label, _)| label == "Signature (simplesigning)")
            .map(|(_, value)| value.clone())
            .expect("the failed leg has a row");
        assert!(
            failed.contains("failed") && failed.contains("registry said no"),
            "a failed leg must name its failure, got {failed:?}"
        );
    }

    // ── CWE-150 — terminal neutralization at the print site ──────────────────

    // One test per attack class, because the classes fail differently — a
    // filter that strips CSI but not a bidi override passes any test written
    // from a single example (SEC-34). Separate named tests rather than
    // `#[rstest] #[case(...)]` rows because `rstest` is not a workspace
    // dependency; a `for` loop over an array would abort at the first failure
    // and report one opaque name, which is the property TEST-04 protects.

    /// A report whose every free-text field carries `hostile`, rendered to the
    /// exact `(label, value)` pairs `print_plain` writes.
    fn rendered_with(hostile: &str) -> Vec<String> {
        let report = SignatureReport::new(
            id(),
            ocx_oci::Digest::Sha256("a".repeat(64)),
            vec![bundle_leg()],
            Some(&"linux/amd64".parse().expect("platform")),
            hostile.to_string(),
            hostile.to_string(),
        );
        report.plain_fields().into_iter().map(|(_, value)| value).collect()
    }

    /// Asserts no cell reaches the terminal carrying an active sequence.
    fn assert_neutralized(hostile: &str) {
        for cell in rendered_with(hostile) {
            assert!(
                !cell.chars().any(|c| c.is_control() || is_bidi_control(c)),
                "sign row {cell:?} reached the terminal unneutralized"
            );
        }
    }

    /// Same check, plus the literal codepoint the payload injected.
    ///
    /// `assert_neutralized` alone asks `is_bidi_control` whether the output is
    /// clean, and the sanitizer filters on that same function -- so narrowing
    /// its range would leave every bidi case green while a raw override
    /// reaches the terminal. The literal is an oracle the production code
    /// cannot move.
    fn assert_neutralized_without(hostile: &str, injected: char) {
        assert_neutralized(hostile);
        for cell in rendered_with(hostile) {
            assert!(!cell.contains(injected), "sign row {cell:?} still carries {injected:?}");
        }
    }

    #[test]
    fn csi_colour_in_a_certificate_field_is_neutralized() {
        assert_neutralized("\u{1b}[31mtrusted@example.com");
    }

    #[test]
    fn osc8_hyperlink_in_a_certificate_field_is_neutralized() {
        assert_neutralized("\u{1b}]8;;https://evil.test\u{7}trusted@example.com\u{1b}]8;;\u{7}");
    }

    #[test]
    fn osc52_clipboard_write_in_a_certificate_field_is_neutralized() {
        // Fulcio's answer decides these strings, not us; the sign path prints
        // them straight back at the operator.
        assert_neutralized("\u{1b}]52;c;ZXZpbA==\u{7}");
    }

    #[test]
    fn bidi_override_in_a_certificate_field_is_neutralized() {
        assert_neutralized_without("\u{202e}moc.elpmaxe@rengis", '\u{202e}');
    }

    #[test]
    fn bidi_isolate_in_a_certificate_field_is_neutralized() {
        assert_neutralized_without("\u{2066}signer@example.com\u{2069}", '\u{2066}');
    }

    #[test]
    fn newline_in_a_field_cannot_forge_a_report_row() {
        assert_neutralized("signer@example.com\nSigner | keyless-fulcio");
    }

    #[test]
    fn nul_in_a_field_is_neutralized() {
        assert_neutralized("signer\u{0}@example.com");
    }

    #[test]
    fn zero_width_and_bom_are_stripped_like_every_other_invisible() {
        // Inverted from the WP9a-era decision that scoped the sanitizer to
        // terminal-active characters only. Invisible is not harmless here:
        // `you@exam\u{200b}ple.com` renders pixel-identical to the identity a
        // reader believes they approved, so SEC-34's set includes them. Pinned
        // per payload so the two surfaces cannot diverge, and so this payload
        // never grows a second hand-rolled filter (SEC-31).
        for (invisible, visible) in [
            ("a\u{200d}b", "ab"),
            ("\u{feff}signer@example.com", "signer@example.com"),
        ] {
            let rows = rendered_with(invisible);
            assert!(
                !rows.iter().any(|cell| cell.contains(invisible)),
                "{invisible:?} survived the sanitizer; got {rows:?}"
            );
            // Positive control: the visible remainder must still arrive, or a
            // sanitizer that dropped the whole field would pass the line above.
            assert!(
                rows.iter().any(|cell| cell == visible),
                "the visible remainder {visible:?} did not reach the report; got {rows:?}"
            );
        }
    }

    #[test]
    fn ordinary_values_pass_through_verbatim() {
        // The neutralization must be invisible for every value `ocx` itself
        // produces — which is what licenses routing the typed digest and
        // platform fields through the same call as the free-text ones.
        let report = sample_report();
        let expected = [
            "registry.example/pkg:1.0".to_string(),
            format!("sha256:{}", "a".repeat(64)),
            "linux/amd64".to_string(),
            "signer@example.com".to_string(),
            "https://accounts.google.com".to_string(),
            "keyless".to_string(),
            // Stated, not omitted: a keyless signature always carries a Rekor
            // entry, and a key-mode one may not — so the row exists in both
            // cases and says which happened.
            "none".to_string(),
            ocx_oci::Digest::Sha256("c".repeat(64)).to_short_string(),
        ];
        let rendered: Vec<String> = report.plain_fields().into_iter().map(|(_, value)| value).collect();
        assert_eq!(rendered, expected, "neutralization must be identity on our own values");
    }

    #[test]
    fn json_keeps_the_certificate_identity_verbatim() {
        // `--format json` is a machine channel: a CI step comparing the
        // identity against its own policy needs the real bytes, and
        // `serde_json` escapes the C0 range by specification.
        let hostile = "\u{1b}]52;c;ZXZpbA==\u{7}signer@example.com";
        let report = SignatureReport::new(
            id(),
            ocx_oci::Digest::Sha256("a".repeat(64)),
            vec![bundle_leg()],
            Some(&"linux/amd64".parse().expect("platform")),
            hostile.to_string(),
            "https://accounts.google.com".into(),
        );
        let json = serde_json::to_string(&report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            parsed["certificate_identity"], hostile,
            "JSON must carry the identity verbatim, not the display form"
        );
        assert!(
            !json.contains('\u{1b}'),
            "serde_json must have escaped the ESC rather than emitting it raw"
        );
    }
}
