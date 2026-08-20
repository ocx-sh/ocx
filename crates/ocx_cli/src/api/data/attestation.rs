// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package attest` output.
//!
//! Sibling of [`SignatureReport`](super::signature::SignatureReport), never a
//! mutation of it: attest and sign publish different referrer bodies and echo
//! different fields, so one shared type would have to make half of them
//! optional.

use ocx_lib::cli::Cell;
use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Summary of a successful keyless attestation.
///
/// Plain format: a single "Field | Value" table. `subject_digest` is the answer
/// (what was attested) and renders full; `bundle_digest` and `referrer_digest`
/// shorten to 12 hex, so one 71-column `sha256:<64hex>` earns its row per view
/// (subsystem-cli-api.md "Plain-Mode Column Budget"). Both stay full in JSON.
///
/// JSON format: `{ identifier, platform, subject_digest, predicate_type,
/// bundle_digest, referrer_digest, certificate_identity,
/// certificate_oidc_issuer }`.
///
/// `predicate_type` is the **resolved** URI, not the `--type` spelling the
/// caller passed: alias resolution decides what is published, annotated and
/// hashed, so echoing it is what keeps the resolution visible rather than
/// surprising (ADR D-c).
#[derive(Serialize)]
pub struct AttestationReport {
    /// User-facing identifier that was attested (echoes the CLI arg).
    pub identifier: String,
    /// Platform whose manifest carries the attestation (e.g. `linux/amd64`).
    pub platform: String,
    /// Digest of the subject manifest the Statement names.
    pub subject_digest: oci::Digest,
    /// The resolved `predicateType` URI written into the Statement.
    pub predicate_type: String,
    /// Digest of the Sigstore bundle blob (the referrer's layer content).
    pub bundle_digest: oci::Digest,
    /// Digest of the published OCI referrer manifest wrapping the bundle.
    pub referrer_digest: oci::Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    pub certificate_identity: String,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert.
    pub certificate_oidc_issuer: String,
}

impl AttestationReport {
    /// Build a report from the pipeline result and the invocation's own inputs.
    ///
    /// Takes the whole [`AttestResult`](ocx_lib::oci::attest::pipeline::AttestResult)
    /// rather than its fields: three of them are `Digest` and two are `String`,
    /// so as adjacent positionals a swapped pair would type-check silently.
    pub fn new(
        identifier: String,
        platform: &oci::Platform,
        result: ocx_lib::oci::attest::pipeline::AttestResult,
    ) -> Self {
        Self {
            identifier,
            platform: platform.to_string(),
            subject_digest: result.subject_digest,
            predicate_type: result.predicate_type,
            bundle_digest: result.bundle_digest,
            referrer_digest: result.referrer_digest,
            certificate_identity: result.certificate_identity,
            certificate_oidc_issuer: result.certificate_oidc_issuer,
        }
    }

    /// The (label, value) pairs `print_plain` renders, in display order.
    ///
    /// Extracted so the digest-shortening contract can be pinned by a unit
    /// test: `Printer` writes to the real process stdout with no injectable
    /// writer, so `print_plain`'s bytes cannot be captured in-process.
    ///
    /// Every value is neutralized for the terminal (CWE-150), not only the
    /// obviously foreign ones — the same position `signature.rs` takes.
    /// `predicate_type` is the sharpest of these: it is attacker-controlled
    /// inside a signed payload, so being authentic says nothing about being
    /// printable. The digests and the platform are typed values that cannot
    /// carry a control character and are routed anyway, because a filter
    /// applied per field has to be re-argued for every field added later.
    fn plain_fields(&self) -> [(&'static str, String); 8] {
        [
            ("Identifier", sanitize_for_terminal(&self.identifier)),
            ("Platform", sanitize_for_terminal(&self.platform)),
            (
                "Subject digest",
                sanitize_for_terminal(&self.subject_digest.to_string()),
            ),
            ("Predicate type", sanitize_for_terminal(&self.predicate_type)),
            (
                "Bundle digest",
                sanitize_for_terminal(&self.bundle_digest.to_short_string()),
            ),
            (
                "Referrer digest",
                sanitize_for_terminal(&self.referrer_digest.to_short_string()),
            ),
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

impl Printable for AttestationReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let mut rows: [Vec<Cell>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in self.plain_fields() {
            rows[0].push(Cell::from(label.to_string()));
            rows[1].push(Cell::from(value));
        }
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }

    /// Emit a C-S1-1 success envelope:
    /// `{"schema_version":1,"command":"package attest","exit_code":0,"data":{...}}`.
    fn print_json(&self, data: &ocx_lib::cli::DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let json = crate::error_envelope::render_success_envelope("package attest", self)?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        Ok(data.print_json(&parsed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::oci::attest::pipeline::AttestResult;

    fn digest(fill: char) -> oci::Digest {
        oci::Digest::Sha256(fill.to_string().repeat(64))
    }

    fn sample() -> AttestationReport {
        AttestationReport::new(
            "registry.example/pkg:1.0".into(),
            &"linux/amd64".parse().expect("platform"),
            AttestResult {
                subject_digest: digest('a'),
                predicate_type: "https://cyclonedx.org/bom".into(),
                bundle_digest: digest('b'),
                referrer_digest: digest('c'),
                referrer_descriptor: Default::default(),
                certificate_identity: "signer@example.com".into(),
                certificate_oidc_issuer: "https://accounts.google.com".into(),
            },
        )
    }

    #[test]
    fn json_output_carries_the_c_s1_1_envelope() {
        let json = crate::error_envelope::render_success_envelope("package attest", &sample()).expect("render");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "package attest");
        assert_eq!(parsed["exit_code"], 0);

        let data = &parsed["data"];
        assert_eq!(data["identifier"], "registry.example/pkg:1.0");
        assert_eq!(
            data["platform"], "linux/amd64",
            "platform must serialize as a plain string"
        );
        assert_eq!(
            data["predicate_type"], "https://cyclonedx.org/bom",
            "the report echoes the RESOLVED predicateType, not the --type spelling"
        );
        // Exact equality, not `starts_with`: a shortened 12-hex form also
        // starts with "sha256:", so only equality pins that JSON never shortens.
        assert_eq!(data["subject_digest"], format!("sha256:{}", "a".repeat(64)));
        assert_eq!(data["bundle_digest"], format!("sha256:{}", "b".repeat(64)));
        assert_eq!(data["referrer_digest"], format!("sha256:{}", "c".repeat(64)));
    }

    /// Plain mode spends its one full-width digest row on the subject — the
    /// answer to "what was attested" — and shortens the other two.
    #[test]
    fn plain_mode_shortens_every_digest_but_the_subject() {
        let report = sample();
        let fields: std::collections::BTreeMap<_, _> = report.plain_fields().into_iter().collect();
        assert_eq!(fields["Subject digest"], format!("sha256:{}", "a".repeat(64)));
        assert_ne!(fields["Bundle digest"], format!("sha256:{}", "b".repeat(64)));
        assert_ne!(fields["Referrer digest"], format!("sha256:{}", "c".repeat(64)));
        assert!(fields["Bundle digest"].len() < 30, "bundle digest was not shortened");
    }

    /// A predicateType is attacker-controlled inside a signed payload: being
    /// authentic says nothing about being printable (CWE-150).
    #[test]
    fn registry_sourced_fields_are_neutralized_for_the_terminal() {
        let mut result = AttestResult {
            subject_digest: digest('a'),
            predicate_type: "https://evil.example/\u{1b}[2Jbom".into(),
            bundle_digest: digest('b'),
            referrer_digest: digest('c'),
            referrer_descriptor: Default::default(),
            certificate_identity: "signer\u{202e}moc.elpmaxe@".into(),
            certificate_oidc_issuer: "https://accounts.google.com".into(),
        };
        result.predicate_type.push('\r');

        let report = AttestationReport::new(
            "registry.example/pkg:1.0".into(),
            &"linux/amd64".parse().unwrap(),
            result,
        );
        for (label, value) in report.plain_fields() {
            assert!(!value.contains('\u{1b}'), "{label} leaked an escape: {value:?}");
            assert!(!value.contains('\r'), "{label} leaked a carriage return: {value:?}");
            assert!(!value.contains('\u{202e}'), "{label} leaked a bidi override: {value:?}");
        }
    }

    /// The neutralization is identity on ordinary values, so routing every
    /// field through it costs nothing readable.
    #[test]
    fn ordinary_values_pass_through_verbatim() {
        let fields: std::collections::BTreeMap<_, _> = sample().plain_fields().into_iter().collect();
        assert_eq!(fields["Identifier"], "registry.example/pkg:1.0");
        assert_eq!(fields["Certificate identity"], "signer@example.com");
        assert_eq!(fields["Predicate type"], "https://cyclonedx.org/bom");
    }

    /// Plain output names the issuer as well as the SAN.
    ///
    /// An identity without its issuer is unattributable: two CAs can vouch for
    /// the same string, and `--certificate-oidc-issuer` is half of what verify
    /// pins. Indexing by label reds if the row is dropped; the length check
    /// reds if a row is dropped elsewhere and this one merely moved.
    #[test]
    fn plain_output_carries_the_certificate_issuer_beside_the_identity() {
        let report = sample();
        let rendered = report.plain_fields();
        assert_eq!(rendered.len(), 8, "a plain row went missing");
        let fields: std::collections::BTreeMap<_, _> = rendered.into_iter().collect();
        assert_eq!(fields["Certificate OIDC issuer"], "https://accounts.google.com");
    }
}
