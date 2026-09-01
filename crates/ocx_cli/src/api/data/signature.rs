// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package sign` output.
//!
//! Renders the subject + referrer descriptors the sign pipeline produced,
//! plus the cert identity/issuer embedded in the signing cert. This lets
//! downstream tools (CI verification, human review) confirm exactly what
//! was signed and by whom without re-fetching the bundle.

// Consumed by `command/package_sign.rs` in Phase 5.

use ocx_lib::cli::{Cell, ExitCode};
use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Summary of a signing operation, keyless or under a key.
///
/// Plain format: single "Field | Value" table listing the identifier, subject
/// digest, one `Signature (<format>)` row per leg, platform, key model, cert
/// identity, and cert OIDC issuer (one row per field — `Printable`
/// single-table rule honored). `subject_digest` is the answer (what was
/// signed) and renders full; a leg's digests shorten to 12 hex — a full
/// `sha256:<64hex>` earns its row only once per view. Everything stays full
/// in JSON.
///
/// A leg's two digests are distinct and not interchangeable: `payload_digest`
/// is the SHA-256 of the signed blob (the Sigstore bundle under `bundle`, the
/// simplesigning claim under `simplesigning` — what the transparency record
/// covers), while `manifest_digest` is the SHA-256 of the manifest it hangs
/// from (the OCI referrer, or the `sha256-<hex>.sig` sidecar). Consumers
/// routinely need one or the other.
///
/// `signer` is the signing mechanism used: `"keyless-fulcio"`, or the key
/// backend's own slug under a key.
#[derive(Serialize, schemars::JsonSchema)]
pub struct SignatureReport {
    /// User-facing identifier string that was signed (echoes the CLI arg).
    pub identifier: String,
    /// Digest of the subject manifest that the bundle signs.
    pub subject_digest: oci::Digest,
    /// One entry per wire shape that was written or attempted, in write order.
    ///
    /// `--signature-format both` emits two **independent** signatures, so the
    /// run is best-effort per leg rather than atomic (spec D8): a leg that
    /// failed is reported alongside one that succeeded, and the exit code comes
    /// from the failure. Hiding the successful leg behind the failure would
    /// leave the operator re-signing what is already published.
    pub legs: Vec<SignatureLegReport>,
    /// Platform narrowed into (e.g., `linux/amd64`), or `any` when
    /// `--platform` was absent and the run signed whatever resolved.
    pub platform: String,
    /// Signing mechanism used (C-S1-1 contract field). Always `"keyless-fulcio"` in Slice 1.
    pub signer: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    pub certificate_identity: String,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert.
    pub certificate_oidc_issuer: String,
    /// Which key model produced this signature (`keyless`, `file`, and — once
    /// they exist — `aws_kms` and friends).
    ///
    /// A consumer can already distinguish `file` from a future `awskms` without
    /// the backends existing, which is the point of freezing the vocabulary
    /// before the implementations (spec §WP9 contract 4).
    pub key_backend: oci::sign::KeyBackendKind,
    /// The signing key's cosign hint, in key mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    pub public_key_hint: Option<String>,
    /// Whether a transparency record was created, and its log index when so.
    ///
    /// **Emitted unconditionally**, `null` included: under a key
    /// `--rekor-upload` is opt-in, so the absence of a record is a legal
    /// outcome the operator must be able to *see* rather than infer from a
    /// missing key.
    pub transparency_log_index: Option<u64>,
    /// The code the process will exit with, for the JSON envelope's own
    /// `exit_code` field. Not part of `data` — it is envelope, not report.
    #[serde(skip)]
    exit_code: ExitCode,
}

/// One wire shape's outcome, as reported.
#[derive(Serialize, schemars::JsonSchema)]
pub struct SignatureLegReport {
    /// The shape: `bundle` or `simplesigning`.
    pub format: oci::sign::SignatureFormat,
    /// Digest of the signed payload blob — the Sigstore bundle under `bundle`,
    /// the simplesigning claim under `simplesigning`. Absent when the leg failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    pub payload_digest: Option<oci::Digest>,
    /// Digest of the manifest the payload hangs from — the OCI referrer under
    /// `bundle`, the `sha256-<hex>.sig` sidecar under `simplesigning`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    pub manifest_digest: Option<oci::Digest>,
    /// Why the leg failed, when it did. `None` means it was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    pub error: Option<String>,
}

/// How the `--platform` request reads in a report when none was made.
///
/// The flag is a narrowing modifier, not a selector, so its absence is a legal
/// outcome the report has to name. `any` is what the absence means — the run
/// put no platform constraint on what it acted on — and it is the same spelling
/// the sign/attest/verify error messages use for the same absence, so a reader
/// meets one word for one state. Keeping the field a plain string is
/// deliberate: it is a shipped JSON contract (C-S1-1), and turning it null
/// would break every consumer reading it unconditionally.
fn platform_label(platform: Option<&oci::Platform>) -> String {
    platform.map_or_else(|| "any".to_string(), oci::Platform::to_string)
}

impl SignatureReport {
    pub fn new(
        identifier: String,
        subject_digest: oci::Digest,
        legs: Vec<SignatureLegReport>,
        platform: Option<&oci::Platform>,
        certificate_identity: String,
        certificate_oidc_issuer: String,
    ) -> Self {
        Self {
            identifier,
            subject_digest,
            legs,
            platform: platform_label(platform),
            signer: "keyless-fulcio".to_string(),
            certificate_identity,
            certificate_oidc_issuer,
            key_backend: oci::sign::KeyBackendKind::Keyless,
            public_key_hint: None,
            transparency_log_index: None,
            exit_code: ExitCode::Success,
        }
    }

    /// Record the code the process exits with, so the envelope agrees with it.
    ///
    /// A `--signature-format both` run where one leg failed still prints this
    /// report — it is the only place the leg that *landed* is named — and then
    /// exits with the failure's code.
    #[must_use]
    pub fn with_exit_code(mut self, exit_code: ExitCode) -> Self {
        self.exit_code = exit_code;
        self
    }

    /// Record which key model signed, and its key hint under a key.
    ///
    /// `signer` moves with it: the field was the constant `"keyless-fulcio"`
    /// while keyless was the only model, and a key-mode signature reported as
    /// `"keyless-fulcio"` would be a lie in the one field a consumer reads to
    /// tell them apart.
    #[must_use]
    pub fn with_key_model(mut self, backend: oci::sign::KeyBackendKind, hint: Option<String>) -> Self {
        self.signer = match backend {
            oci::sign::KeyBackendKind::Keyless => "keyless-fulcio".to_string(),
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
    /// The (label, value) pairs `print_plain` renders, in display order.
    ///
    /// Extracted from `print_plain` so the digest-shortening contract can be
    /// pinned by a unit test: `Printer` writes directly to the real process
    /// stdout with no injectable writer (see `data_interface.rs`), so
    /// `print_plain`'s rendered bytes cannot be captured in-process. This pure
    /// helper carries the same field list with no `Printer` dependency.
    /// Every value is neutralized for the terminal (CWE-150), not only the two
    /// obviously-foreign ones. `certificate_identity` and
    /// `certificate_oidc_issuer` are read out of the Fulcio certificate this
    /// run received, so their content is the certificate authority's answer
    /// rather than ours. `identifier` reaches here as argv, which is still not
    /// operator-authored under `ocx exec` and a script-supplied identifier —
    /// the same position `command/index_common.rs` already takes. The digests
    /// and the platform are typed values that cannot carry a control
    /// character, and are routed anyway: a filter applied per field has to be
    /// re-argued for every field added later, and the neutralization is
    /// identity on them — pinned by `ordinary_values_pass_through_verbatim`.
    fn plain_fields(&self) -> Vec<(String, String)> {
        let mut fields = vec![
            ("Identifier".to_string(), sanitize_for_terminal(&self.identifier)),
            (
                "Subject digest".to_string(),
                sanitize_for_terminal(&self.subject_digest.to_string()),
            ),
            ("Platform".to_string(), sanitize_for_terminal(&self.platform)),
            (
                "Certificate identity".to_string(),
                sanitize_for_terminal(&self.certificate_identity),
            ),
            (
                "Certificate OIDC issuer".to_string(),
                sanitize_for_terminal(&self.certificate_oidc_issuer),
            ),
            (
                "Key backend".to_string(),
                sanitize_for_terminal(&self.key_backend.to_string()),
            ),
            // Stated, never omitted: a missing transparency record is a legal
            // outcome under a key, and an operator must be able to read it off
            // the result rather than infer it from an absent row.
            (
                "Transparency log".to_string(),
                match self.transparency_log_index {
                    Some(index) => format!("Rekor index {index}"),
                    None => "none".to_string(),
                },
            ),
        ];
        // One row per leg, so `--signature-format both` shows two outcomes
        // rather than one and an omission. A failed leg says so in the value
        // instead of vanishing.
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

impl Printable for SignatureReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        // `subject_digest` is the answer (what was signed) and stays full;
        // `bundle_digest`/`referrer_digest` shorten to 12 hex so only one
        // full sha256:<64hex> earns its row (subsystem-cli-api.md "Plain-Mode
        // Column Budget"). `signer` has no row — it is the constant
        // "keyless-fulcio" for Slice 1. Both remain full/present in JSON.
        let mut rows: [Vec<Cell>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in self.plain_fields() {
            rows[0].push(Cell::from(label));
            rows[1].push(Cell::from(value));
        }
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }

    /// Emit a C-S1-1 envelope:
    /// `{"schema_version":1,"command":"package sign","exit_code":<code>,"data":{...}}`.
    ///
    /// `exit_code` is 0 for a run where every leg landed, and the failing leg's
    /// code for a partial `--signature-format both` run — the same value the
    /// process returns.
    fn print_json(&self, data: &ocx_lib::cli::DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let parsed: serde_json::Value = serde_json::from_str(&self.envelope()?)?;
        Ok(data.print_json(&parsed)?)
    }
}

impl SignatureReport {
    /// The envelope document `print_json` emits.
    ///
    /// Separate from `print_json` only because `DataInterface` writes to the
    /// process's stdout, so a test can reach the rendered bytes no other way —
    /// and the envelope's `exit_code` is a claim about the process that has to
    /// be checked against a report that failed a leg.
    fn envelope(&self) -> anyhow::Result<String> {
        crate::error_envelope::render_envelope_with_exit_code("package sign", self, self.exit_code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::data::is_bidi_control;

    /// One written `bundle` leg — the default `--signature-format`.
    fn bundle_leg() -> SignatureLegReport {
        SignatureLegReport {
            format: oci::sign::SignatureFormat::Bundle,
            payload_digest: Some(ocx_lib::oci::Digest::Sha256("b".repeat(64))),
            manifest_digest: Some(ocx_lib::oci::Digest::Sha256("c".repeat(64))),
            error: None,
        }
    }

    fn sample_report() -> SignatureReport {
        SignatureReport::new(
            "registry.example/pkg:1.0".into(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            vec![bundle_leg()],
            Some(&"linux/amd64".parse().expect("platform")),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
        )
    }

    /// A run where the simplesigning leg failed and the bundle leg landed.
    fn partially_failed_report() -> SignatureReport {
        SignatureReport::new(
            "registry.example/pkg:1.0".into(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            vec![
                bundle_leg(),
                SignatureLegReport {
                    format: oci::sign::SignatureFormat::Simplesigning,
                    payload_digest: None,
                    manifest_digest: None,
                    error: Some("transient registry failure".into()),
                },
            ],
            Some(&"linux/amd64".parse().expect("platform")),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
        )
        .with_exit_code(ExitCode::TempFail)
    }

    /// The envelope of a partial `--signature-format both` run must report the
    /// code the process exits with.
    ///
    /// `error_envelope.rs` states the invariant this defends — the envelope's
    /// `exit_code` can never disagree with the process's — and a success
    /// envelope hard-codes 0, so a report-then-fail command needs the other
    /// renderer or it ships a `"exit_code":0` in front of a non-zero `$?`.
    #[test]
    fn a_partial_run_envelope_reports_the_failing_legs_exit_code() {
        let json = partially_failed_report().envelope().expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 75, "TempFail is 75: {parsed}");
        // The leg that landed is still reported: hiding it would leave the
        // operator re-signing what is already published.
        assert_eq!(
            parsed["data"]["legs"][0]["manifest_digest"],
            format!("sha256:{}", "c".repeat(64))
        );
        assert!(parsed["data"]["legs"][1]["error"].is_string());
    }

    #[test]
    fn json_output_contains_c_s1_1_envelope() {
        // `DataInterface` writes to the process's stdout rather than a buffer,
        // so the rendered document is reached through the same `envelope()`
        // helper `print_json` parses — one call away from the printed bytes.
        let report = sample_report();
        let json = report.envelope().expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "package sign");
        assert_eq!(parsed["exit_code"], 0);
        let data = &parsed["data"];
        assert_eq!(data["identifier"], "registry.example/pkg:1.0");
        // JSON keeps every digest full, unlike plain mode — a shortened 12-hex
        // form also satisfies `starts_with("sha256:")`, so exact equality is
        // required to pin that JSON never shortens (see
        // `print_plain_shortens_bundle_and_referrer_digests_but_not_subject`
        // for the plain-mode counterpart).
        assert_eq!(data["subject_digest"], format!("sha256:{}", "a".repeat(64)));
        assert_eq!(data["legs"][0]["format"], "bundle");
        assert_eq!(data["legs"][0]["payload_digest"], format!("sha256:{}", "b".repeat(64)));
        assert_eq!(data["legs"][0]["manifest_digest"], format!("sha256:{}", "c".repeat(64)));
        // C-S1-1 contract: platform must serialize as a plain string (e.g. "linux/amd64").
        assert_eq!(data["platform"], "linux/amd64", "data[platform] must be a plain string");
        assert_eq!(data["signer"], "keyless-fulcio");
    }

    /// `print_plain` shortens `bundle_digest`/`referrer_digest` to 12 hex (only
    /// `subject_digest` earns a full `sha256:<64hex>` row) and drops `signer`
    /// (constant for Slice 1) — smoke-checks the table renders without panic.
    #[test]
    fn print_plain_smoke() {
        let report = sample_report();
        let data = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
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
            ocx_lib::oci::Digest::Sha256("c".repeat(64)).to_short_string(),
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
            "registry.example/pkg:1.0".into(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            vec![
                bundle_leg(),
                SignatureLegReport {
                    format: oci::sign::SignatureFormat::Simplesigning,
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
            hostile.to_string(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
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
            ocx_lib::oci::Digest::Sha256("c".repeat(64)).to_short_string(),
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
            "registry.example/pkg:1.0".into(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            vec![bundle_leg()],
            Some(&"linux/amd64".parse().expect("platform")),
            hostile.to_string(),
            "https://accounts.google.com".into(),
        );
        let json = crate::error_envelope::render_success_envelope("package sign", &report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            parsed["data"]["certificate_identity"], hostile,
            "JSON must carry the identity verbatim, not the display form"
        );
        assert!(
            !json.contains('\u{1b}'),
            "serde_json must have escaped the ESC rather than emitting it raw"
        );
    }
}
