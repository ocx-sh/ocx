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
/// bundle_digest, referrer_digest, sidecar_digest, certificate_identity,
/// certificate_oidc_issuer }`.
///
/// `bundle_digest` and `referrer_digest` describe the OCI 1.1 referrer and are
/// absent under `--signature-format simplesigning`, which publishes only the
/// `sha256-<hex>.att` sidecar `sidecar_digest` names. Their shipped spelling is
/// kept: an invocation that does not pass `--signature-format` sees exactly the
/// keys it always did.
///
/// `predicate_type` is the **resolved** URI, not the `--type` spelling the
/// caller passed: alias resolution decides what is published, annotated and
/// hashed, so echoing it is what keeps the resolution visible rather than
/// surprising (ADR D-c).
#[derive(Serialize)]
pub struct AttestationReport {
    /// User-facing identifier that was attested (echoes the CLI arg).
    pub identifier: String,
    /// Platform narrowed into (e.g. `linux/amd64`), or `any` when
    /// `--platform` was absent and the run attested whatever resolved.
    pub platform: String,
    /// Digest of the subject manifest the Statement names.
    pub subject_digest: oci::Digest,
    /// The resolved `predicateType` URI written into the Statement.
    pub predicate_type: String,
    /// Digest of the referrer's layer content: the Sigstore bundle blob on a
    /// signed attach, the SBOM document itself on an unsigned one. The JSON key
    /// keeps its shipped name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_digest: Option<oci::Digest>,
    /// Digest of the published OCI referrer manifest wrapping the payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer_digest: Option<oci::Digest>,
    /// Digest of the `sha256-<hex>.att` sidecar manifest, when
    /// `--signature-format` asked for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidecar_digest: Option<oci::Digest>,
    /// Whether the referrer carries a signature. `false` means the document was
    /// attached as-is, with no identity behind it — the two certificate fields
    /// below are then absent rather than empty.
    pub signed: bool,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Which key model produced this attestation (`keyless`, `file`, and —
    /// once they exist — `aws_kms` and friends). Absent on an unsigned attach,
    /// where no key model was involved at all.
    ///
    /// Spelled as [`SignatureReport`](super::signature::SignatureReport)
    /// spells it, so one vocabulary describes both commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_backend: Option<oci::sign::KeyBackendKind>,
    /// The signing key's cosign hint, in key mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_hint: Option<String>,
    /// Whether a transparency record was created, and its log index when so.
    ///
    /// **Emitted unconditionally**, `null` included, exactly as the sign report
    /// emits it: under a key `--rekor-upload` is opt-in, so a missing Rekor
    /// entry is a legal outcome the operator has to be able to *see* rather
    /// than infer from a key that is not there.
    pub transparency_log_index: Option<u64>,
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

impl AttestationReport {
    /// Build a report from the pipeline result and the invocation's own inputs.
    ///
    /// Takes the whole [`AttestResult`](ocx_lib::oci::attest::pipeline::AttestResult)
    /// rather than its fields: three of them are `Digest` and two are `String`,
    /// so as adjacent positionals a swapped pair would type-check silently.
    pub fn new(
        identifier: String,
        platform: Option<&oci::Platform>,
        result: ocx_lib::oci::attest::pipeline::AttestResult,
    ) -> Self {
        Self {
            identifier,
            platform: platform_label(platform),
            subject_digest: result.subject_digest,
            predicate_type: result.predicate_type,
            bundle_digest: result.referrer.as_ref().map(|leg| leg.payload_digest.clone()),
            referrer_digest: result.referrer.map(|leg| leg.manifest_digest),
            sidecar_digest: result.sidecar.map(|leg| leg.manifest_digest),
            signed: result.signed,
            certificate_identity: result.certificate_identity,
            certificate_oidc_issuer: result.certificate_oidc_issuer,
            key_backend: result.key_backend,
            public_key_hint: result.public_key_hint,
            transparency_log_index: result.transparency_log_index,
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
    fn plain_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![
            ("Identifier", sanitize_for_terminal(&self.identifier)),
            ("Platform", sanitize_for_terminal(&self.platform)),
            (
                "Subject digest",
                sanitize_for_terminal(&self.subject_digest.to_string()),
            ),
            ("Predicate type", sanitize_for_terminal(&self.predicate_type)),
            // Stated outright rather than left to be inferred from the absence
            // of the two certificate rows below: an operator scanning a table
            // notices a row that says "unsigned", and does not notice two rows
            // that are not there.
            (
                "Signature",
                match self.signed {
                    true => "signed".to_string(),
                    false => "unsigned (attached without an identity)".to_string(),
                },
            ),
        ];
        for (label, digest) in [
            ("Payload digest", self.bundle_digest.as_ref()),
            ("Referrer digest", self.referrer_digest.as_ref()),
            ("Sidecar digest", self.sidecar_digest.as_ref()),
        ] {
            if let Some(digest) = digest {
                fields.push((label, sanitize_for_terminal(&digest.to_short_string())));
            }
        }
        if let Some(identity) = &self.certificate_identity {
            fields.push(("Certificate identity", sanitize_for_terminal(identity)));
        }
        if let Some(issuer) = &self.certificate_oidc_issuer {
            fields.push(("Certificate OIDC issuer", sanitize_for_terminal(issuer)));
        }
        if let Some(backend) = &self.key_backend {
            fields.push(("Key backend", sanitize_for_terminal(&backend.to_string())));
        }
        // Stated, never omitted, for the reason `signature.rs` states at its
        // twin: under a key `--rekor-upload` is opt-in, so "no record" has to
        // be readable off the result instead of inferred from an absent row.
        fields.push((
            "Transparency log",
            match self.transparency_log_index {
                Some(index) => format!("Rekor index {index}"),
                None => "none".to_string(),
            },
        ));
        fields
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
    use ocx_lib::oci::sign::pipeline::LegDigests;

    fn digest(fill: char) -> oci::Digest {
        oci::Digest::Sha256(fill.to_string().repeat(64))
    }

    fn sample() -> AttestationReport {
        AttestationReport::new(
            "registry.example/pkg:1.0".into(),
            Some(&"linux/amd64".parse().expect("platform")),
            AttestResult {
                key_backend: None,
                public_key_hint: None,
                transparency_log_index: None,
                subject_digest: digest('a'),
                predicate_type: "https://cyclonedx.org/bom".into(),
                referrer: Some(LegDigests {
                    payload_digest: digest('b'),
                    manifest_digest: digest('c'),
                }),
                sidecar: None,
                signed: true,
                certificate_identity: Some("signer@example.com".into()),
                certificate_oidc_issuer: Some("https://accounts.google.com".into()),
            },
        )
    }

    /// The unsigned twin: same attach, no identity behind it.
    fn unsigned_sample() -> AttestationReport {
        AttestationReport::new(
            "registry.example/pkg:1.0".into(),
            Some(&"linux/amd64".parse().expect("platform")),
            AttestResult {
                key_backend: None,
                public_key_hint: None,
                transparency_log_index: None,
                subject_digest: digest('a'),
                predicate_type: "https://cyclonedx.org/bom".into(),
                referrer: Some(LegDigests {
                    payload_digest: digest('b'),
                    manifest_digest: digest('c'),
                }),
                sidecar: None,
                signed: false,
                certificate_identity: None,
                certificate_oidc_issuer: None,
            },
        )
    }

    /// An unsigned attach reports `signed: false` and omits both certificate
    /// keys rather than emitting them empty — an empty SAN reads as an identity
    /// that failed to render, which is the opposite of what happened.
    #[test]
    fn an_unsigned_attach_omits_the_certificate_keys_and_says_so() {
        let json =
            crate::error_envelope::render_success_envelope("package attest", &unsigned_sample()).expect("render");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        let data = &parsed["data"];

        assert_eq!(data["signed"], false);
        assert!(data.get("certificate_identity").is_none(), "empty SAN would mislead");
        assert!(data.get("certificate_oidc_issuer").is_none());
        // The positive control: the signed shape still carries both, so this
        // pair cannot pass by the keys having been dropped everywhere.
        let signed = crate::error_envelope::render_success_envelope("package attest", &sample()).expect("render");
        let signed: serde_json::Value = serde_json::from_str(&signed).expect("valid json");
        assert_eq!(signed["data"]["signed"], true);
        assert_eq!(signed["data"]["certificate_identity"], "signer@example.com");
    }

    /// A key-mode attest with `--rekor-upload` off: the report says which key
    /// model signed **and** that no transparency record exists.
    ///
    /// The spec is explicit — "a missing Rekor entry must be a fact the
    /// operator can see, not an omission they infer" — so
    /// `transparency_log_index` is emitted as `null` rather than skipped, and
    /// the plain table keeps a "Transparency log: none" row. Before this the
    /// report carried none of the three fields, so `attest --key` gave an
    /// operator no way to tell a logged signature from an unlogged one.
    ///
    /// The `Some(index)` half runs beside it so neither can pass by the key
    /// having been dropped for both.
    #[test]
    fn a_key_mode_attest_reports_the_key_model_and_whether_a_record_exists() {
        let report = |transparency_log_index| {
            AttestationReport::new(
                "registry.example/pkg:1.0".into(),
                None,
                AttestResult {
                    key_backend: Some(ocx_lib::oci::sign::KeyBackendKind::File),
                    public_key_hint: Some("cosign-hint".into()),
                    transparency_log_index,
                    subject_digest: digest('a'),
                    predicate_type: "https://cyclonedx.org/bom".into(),
                    referrer: Some(LegDigests {
                        payload_digest: digest('b'),
                        manifest_digest: digest('c'),
                    }),
                    sidecar: None,
                    signed: true,
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                },
            )
        };
        let render = |report: &AttestationReport| {
            let json = crate::error_envelope::render_success_envelope("package attest", report).expect("render");
            serde_json::from_str::<serde_json::Value>(&json).expect("valid json")
        };

        let unlogged = report(None);
        let data = &render(&unlogged)["data"];
        assert_eq!(data["key_backend"], "file");
        assert_eq!(data["public_key_hint"], "cosign-hint");
        assert!(
            data.get("transparency_log_index")
                .is_some_and(serde_json::Value::is_null),
            "a missing Rekor entry must be stated as null, never omitted: {data}"
        );
        let plain: std::collections::BTreeMap<_, _> = unlogged.plain_fields().into_iter().collect();
        assert_eq!(plain["Key backend"], "file");
        assert_eq!(plain["Transparency log"], "none");

        let logged = report(Some(1234));
        assert_eq!(render(&logged)["data"]["transparency_log_index"], 1234);
        let plain: std::collections::BTreeMap<_, _> = logged.plain_fields().into_iter().collect();
        assert_eq!(plain["Transparency log"], "Rekor index 1234");
    }

    /// The unsigned twin: no key model was involved, so `key_backend` is
    /// omitted rather than defaulted to `keyless` — which would name a
    /// signing model for a document nothing signed.
    #[test]
    fn an_unsigned_attach_omits_the_key_backend_but_still_states_the_record() {
        let json =
            crate::error_envelope::render_success_envelope("package attest", &unsigned_sample()).expect("render");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        let data = &parsed["data"];
        assert!(data.get("key_backend").is_none(), "nothing signed it: {data}");
        assert!(data.get("public_key_hint").is_none());
        assert!(
            data.get("transparency_log_index")
                .is_some_and(serde_json::Value::is_null)
        );
    }

    /// Plain output names the trust class outright. Inferring it from two
    /// missing rows is exactly the reading an operator does not do.
    #[test]
    fn plain_output_states_whether_the_attach_was_signed() {
        let signed: std::collections::BTreeMap<_, _> = sample().plain_fields().into_iter().collect();
        assert_eq!(signed["Signature"], "signed");
        assert!(signed.contains_key("Certificate identity"));

        let unsigned: std::collections::BTreeMap<_, _> = unsigned_sample().plain_fields().into_iter().collect();
        assert!(
            unsigned["Signature"].contains("unsigned"),
            "got {:?}",
            unsigned["Signature"]
        );
        assert!(
            !unsigned.contains_key("Certificate identity"),
            "an unsigned attach has no certificate to name"
        );
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
        assert_ne!(fields["Payload digest"], format!("sha256:{}", "b".repeat(64)));
        assert_ne!(fields["Referrer digest"], format!("sha256:{}", "c".repeat(64)));
        assert!(fields["Payload digest"].len() < 30, "payload digest was not shortened");
    }

    /// A predicateType is attacker-controlled inside a signed payload: being
    /// authentic says nothing about being printable (CWE-150).
    #[test]
    fn registry_sourced_fields_are_neutralized_for_the_terminal() {
        let mut result = AttestResult {
            key_backend: None,
            public_key_hint: None,
            transparency_log_index: None,
            subject_digest: digest('a'),
            predicate_type: "https://evil.example/\u{1b}[2Jbom".into(),
            referrer: Some(LegDigests {
                payload_digest: digest('b'),
                manifest_digest: digest('c'),
            }),
            sidecar: None,
            signed: true,
            certificate_identity: Some("signer\u{202e}moc.elpmaxe@".into()),
            certificate_oidc_issuer: Some("https://accounts.google.com".into()),
        };
        result.predicate_type.push('\r');

        let report = AttestationReport::new(
            "registry.example/pkg:1.0".into(),
            Some(&"linux/amd64".parse().unwrap()),
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
        // 10 = the eight this sample always had, plus the unconditional
        // "Transparency log" row. `sample()` carries no `key_backend`, so its
        // conditional row is absent here and covered by the key-mode test
        // above.
        assert_eq!(rendered.len(), 10, "a plain row went missing");
        let fields: std::collections::BTreeMap<_, _> = rendered.into_iter().collect();
        assert_eq!(fields["Certificate OIDC issuer"], "https://accounts.google.com");
    }
}
