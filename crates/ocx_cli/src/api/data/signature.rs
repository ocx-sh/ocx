// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package sign` output.
//!
//! Renders the subject + referrer descriptors the sign pipeline produced,
//! plus the cert identity/issuer embedded in the signing cert. This lets
//! downstream tools (CI verification, human review) confirm exactly what
//! was signed and by whom without re-fetching the bundle.

// Consumed by `command/package_sign.rs` in Phase 5.

use ocx_lib::cli::Cell;
use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Summary of a successful keyless signing operation.
///
/// Plain format: single "Field | Value" table listing the identifier, subject
/// digest, bundle digest, referrer digest, platform, cert identity, and cert
/// OIDC issuer (one row per field — `Printable` single-table rule honored).
/// `subject_digest` is the answer (what was signed) and renders full;
/// `bundle_digest` and `referrer_digest` shorten to 12 hex — a full
/// `sha256:<64hex>` earns its row only once per view. `signer` has no row:
/// for Slice 1 it is always the constant `"keyless-fulcio"`. Both stay full
/// (and `signer` stays present) in JSON.
///
/// JSON format: `{ identifier, subject_digest, bundle_digest, referrer_digest,
/// platform, signer, certificate_identity, certificate_oidc_issuer }`.
///
/// `bundle_digest` and `referrer_digest` are distinct and not interchangeable:
/// the former is the SHA-256 of the Sigstore bundle v0.3 protobuf blob (the
/// layer content; what Rekor's inclusion proof covers), while the latter is
/// the SHA-256 of the OCI referrer manifest JSON (what the Referrers API
/// returns). Consumers routinely need one or the other.
///
/// `signer` is the signing mechanism used; for Slice 1 this is always
/// `"keyless-fulcio"`.
#[derive(Serialize)]
pub struct SignatureReport {
    /// User-facing identifier string that was signed (echoes the CLI arg).
    pub identifier: String,
    /// Digest of the subject manifest that the bundle signs.
    pub subject_digest: oci::Digest,
    /// Digest of the Sigstore bundle blob (the referrer's layer content).
    pub bundle_digest: oci::Digest,
    /// Digest of the published OCI referrer manifest wrapping the bundle.
    pub referrer_digest: oci::Digest,
    /// Platform that was signed (e.g., `linux/amd64`).
    pub platform: String,
    /// Signing mechanism used (C-S1-1 contract field). Always `"keyless-fulcio"` in Slice 1.
    pub signer: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    pub certificate_identity: String,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert.
    pub certificate_oidc_issuer: String,
}

impl SignatureReport {
    pub fn new(
        identifier: String,
        subject_digest: oci::Digest,
        bundle_digest: oci::Digest,
        referrer_digest: oci::Digest,
        platform: &oci::Platform,
        certificate_identity: String,
        certificate_oidc_issuer: String,
    ) -> Self {
        Self {
            identifier,
            subject_digest,
            bundle_digest,
            referrer_digest,
            platform: platform.to_string(),
            signer: "keyless-fulcio".to_string(),
            certificate_identity,
            certificate_oidc_issuer,
        }
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
    /// operator-authored under `ocx run` and a script-supplied identifier —
    /// the same position `command/index_common.rs` already takes. The digests
    /// and the platform are typed values that cannot carry a control
    /// character, and are routed anyway: a filter applied per field has to be
    /// re-argued for every field added later, and the neutralization is
    /// identity on them — pinned by `ordinary_values_pass_through_verbatim`.
    fn plain_fields(&self) -> [(&'static str, String); 7] {
        [
            ("Identifier", sanitize_for_terminal(&self.identifier)),
            (
                "Subject digest",
                sanitize_for_terminal(&self.subject_digest.to_string()),
            ),
            (
                "Bundle digest",
                sanitize_for_terminal(&self.bundle_digest.to_short_string()),
            ),
            (
                "Referrer digest",
                sanitize_for_terminal(&self.referrer_digest.to_short_string()),
            ),
            ("Platform", sanitize_for_terminal(&self.platform)),
            (
                "Certificate identity",
                sanitize_for_terminal(&self.certificate_identity),
            ),
            (
                "Certificate OIDC issuer",
                sanitize_for_terminal(&self.certificate_oidc_issuer),
            ),
        ]
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
            rows[0].push(Cell::from(label.to_string()));
            rows[1].push(Cell::from(value));
        }
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }

    /// Emit a C-S1-1 success envelope:
    /// `{"schema_version":1,"command":"package sign","exit_code":0,"data":{...}}`.
    fn print_json(&self, data: &ocx_lib::cli::DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let json = crate::error_envelope::render_success_envelope("package sign", self)?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        Ok(data.print_json(&parsed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::data::is_bidi_control;

    fn sample_report() -> SignatureReport {
        SignatureReport::new(
            "registry.example/pkg:1.0".into(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            ocx_lib::oci::Digest::Sha256("b".repeat(64)),
            ocx_lib::oci::Digest::Sha256("c".repeat(64)),
            &"linux/amd64".parse().expect("platform"),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
        )
    }

    #[test]
    fn json_output_contains_c_s1_1_envelope() {
        // Capture stdout via a custom test: we call render_success_envelope directly
        // since Printer writes to stdout (not a buffer). This unit test validates
        // that print_json delegates through render_success_envelope correctly.
        let report = sample_report();
        let json = crate::error_envelope::render_success_envelope("package sign", &report).expect("render ok");
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
        assert_eq!(data["bundle_digest"], format!("sha256:{}", "b".repeat(64)));
        assert_eq!(data["referrer_digest"], format!("sha256:{}", "c".repeat(64)));
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
    fn print_plain_shortens_bundle_and_referrer_digests_but_not_subject() {
        let report = sample_report();
        let fields = report.plain_fields();

        let value_for = |label: &str| -> String {
            fields
                .iter()
                .find(|(field_label, _)| *field_label == label)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("missing field {label:?} in plain_fields()"))
        };

        let full_subject = format!("sha256:{}", "a".repeat(64));
        let full_bundle = format!("sha256:{}", "b".repeat(64));
        let full_referrer = format!("sha256:{}", "c".repeat(64));

        assert_eq!(
            value_for("Subject digest"),
            full_subject,
            "subject digest must stay full"
        );
        assert_eq!(
            value_for("Bundle digest"),
            report.bundle_digest.to_short_string(),
            "bundle digest must shorten to 12 hex"
        );
        assert_ne!(
            value_for("Bundle digest"),
            full_bundle,
            "bundle digest must not render the full 64-hex form"
        );
        assert_eq!(
            value_for("Referrer digest"),
            report.referrer_digest.to_short_string(),
            "referrer digest must shorten to 12 hex"
        );
        assert_ne!(
            value_for("Referrer digest"),
            full_referrer,
            "referrer digest must not render the full 64-hex form"
        );
        assert!(
            fields.iter().all(|(label, _)| *label != "Signer"),
            "signer has no row in plain mode (constant for Slice 1); found row: {fields:?}"
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
            ocx_lib::oci::Digest::Sha256("b".repeat(64)),
            ocx_lib::oci::Digest::Sha256("c".repeat(64)),
            &"linux/amd64".parse().expect("platform"),
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
            report.bundle_digest.to_short_string(),
            report.referrer_digest.to_short_string(),
            "linux/amd64".to_string(),
            "signer@example.com".to_string(),
            "https://accounts.google.com".to_string(),
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
            ocx_lib::oci::Digest::Sha256("b".repeat(64)),
            ocx_lib::oci::Digest::Sha256("c".repeat(64)),
            &"linux/amd64".parse().expect("platform"),
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
