// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package sbom` — list, extract or summarize the verified SBOM
//! attestations a published package carries.
//!
//! The attestation twin of `ocx package verify`: same identity resolution, same
//! trust-root ladder, same pipeline — different arity. Verification is
//! unconditional and there is no `--no-verify`: an unverified listing is
//! registry-controlled text presented as fact, which is the shape SEC-32 exists
//! to prevent.
//!
//! Three modes, mutually exclusive by construction (clap `conflicts_with`):
//! the default listing, `--output PATH` writing one verified predicate
//! verbatim, and `--summary` parsing each CycloneDX document.
//!
//! `--output -` refuses a TTY. The predicate is authored by whoever holds an
//! identity the policy admits, so "verified" does not mean "safe to print":
//! written verbatim to a terminal, a component description carrying an OSC 52
//! sequence sets the operator's clipboard (CWE-150). The bytes must stay exact
//! for the round-trip contract, so the terminal is declined instead.

use std::collections::BTreeSet;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use tokio::io::AsyncWriteExt as _;

use ocx_lib::cli;
use ocx_lib::cli::ClassifyErrorKind as _;
use ocx_lib::oci;
use ocx_lib::oci::attest::predicate::PredicateType;
use ocx_lib::oci::verify::{AttestationMatch, RefusedCandidate, VerifyError, VerifyErrorKind};
use ocx_lib::package_manager::SbomOptions;
use ocx_lib::sbom;

use crate::api::data::sbom::{RefusedEntry, SbomEntry, SbomListingReport, SbomSummaryOut};
use crate::app::CommandError;
use crate::command::package_sign_common;
use crate::options;

/// List the verified SBOM attestations a published package carries.
#[derive(Parser, Clone)]
pub struct PackageSbom {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    /// Write the verified predicate document to PATH ("-" for stdout).
    ///
    /// The bytes are the exact sub-slice the publisher signed, never a
    /// re-serialization. More than one verified attestation of the requested
    /// type is a refusal, not a choice: narrow with --type. Writing raw
    /// predicate bytes to a terminal is refused - redirect to a file or a pipe.
    #[clap(long = "output", short = 'o', value_name = "PATH", conflicts_with = "summary")]
    output: Option<PathBuf>,

    /// Parse each SBOM and report component counts (CycloneDX 1.5-1.7 only).
    ///
    /// A predicate outside that range refuses that entry - it moves to the
    /// refused list, naming the type it could not read - never an empty
    /// summary, and never the rest of the listing. The listing still works
    /// without this flag.
    #[clap(long = "summary", conflicts_with = "output")]
    summary: bool,

    /// Restrict to one predicate type (for example cyclonedx or spdx).
    #[clap(long = "type", value_name = "TYPE")]
    predicate_type: Option<PredicateType>,

    /// Expected certificate SAN (exact match).
    ///
    /// Optional when a `[trust.policy]` whose scope covers the target supplies
    /// the identity; when given, this flag and `--certificate-oidc-issuer`
    /// override any policy. The two flags are used together; supplying one
    /// without the other is an error.
    #[clap(
        long = "certificate-identity",
        value_name = "IDENTITY",
        requires = "certificate_oidc_issuer"
    )]
    certificate_identity: Option<String>,

    /// Expected certificate OIDC issuer (exact match).
    ///
    /// Optional when a matching `[trust.policy]` supplies the issuer; used
    /// together with `--certificate-identity` to override any policy.
    #[clap(
        long = "certificate-oidc-issuer",
        value_name = "URL",
        requires = "certificate_identity"
    )]
    certificate_oidc_issuer: Option<String>,

    /// Trust-root override: a Sigstore trusted-root JSON (or a directory holding
    /// trusted_root.json).
    ///
    /// Supplies the Fulcio CA, the CT-log key and the pinned Rekor public key
    /// for air-gapped verification against a local trust-root mirror. No TUF
    /// network fetch is performed. Takes precedence over the
    /// OCX_SIGSTORE_TRUSTED_ROOT env var and over [trust.sigstore] in
    /// config.toml. See
    /// https://ocx.sh/docs/in-depth/self-hosted-sigstore
    #[clap(long = "sigstore-trusted-root", value_name = "PATH")]
    trusted_root: Option<PathBuf>,

    /// Rekor transparency-log endpoint
    ///
    /// Defaults to [trust.sigstore].rekor_url, else public Rekor.
    #[clap(long = "rekor-url", value_name = "URL")]
    rekor_url: Option<String>,

    /// Bypass the referrers-capability cache for this invocation.
    #[clap(long = "no-cache")]
    no_cache: bool,

    /// Package identifier to read (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

/// `reason_kind` for a verified attestation `--summary` could not parse.
///
/// Deliberately outside the `VerifyErrorKind::kind_detail` set every other
/// refusal slug comes from: the bundle verified and the signature held, and
/// only the reading of the payload failed. A script that treats a refused
/// signature and an unreadable SBOM as the same event is drawing the wrong
/// conclusion about the publisher.
const SUMMARY_FAILED: &str = "sbom_summary_failed";

/// Where `--output` sends the verified predicate bytes.
///
/// A parsed destination rather than a bare `Option<PathBuf>`, so the TTY
/// refusal is a decision over a value and can be tested without a terminal
/// (ARCH-12).
#[derive(Debug, Clone, PartialEq, Eq)]
enum OutputDestination {
    /// `-` — the process's own stdout.
    Stdout,
    /// A filesystem path.
    File(PathBuf),
}

impl PackageSbom {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // SSRF hardening (CWE-918): validate the user-supplied endpoint at the
        // boundary before it becomes an HTTP client target. Precedence, guard
        // and refusal kind are the shared ladder's — the same one `verify`
        // walks, which is the point: the two commands key the same trust-root
        // cache and must not disagree about which Rekor they mean.
        let rekor_url = package_sign_common::resolve_rekor_endpoint(
            context.config_trust_sigstore(),
            &identifier,
            self.rekor_url.as_deref(),
        )?;

        let destination = destination(self.output.as_deref());
        // Refuse before the network round-trip, not after: an operator whose
        // invocation cannot succeed should learn it in milliseconds.
        if let Some(destination) = &destination {
            refuse_tty_output(destination, std::io::stdout().is_terminal())?;
        }

        let client = context.verify_client();
        let offline = context.is_offline();
        let rekor_cache_key = ocx_lib::oci::verify::trust_cache::cache_key_for_rekor(&rekor_url);
        let trust_root = package_sign_common::resolve_trust_root(
            &context,
            &identifier,
            &rekor_cache_key,
            offline,
            self.trusted_root.as_deref(),
        )
        .await?;
        let policies = package_sign_common::resolve_policies(
            &context,
            &identifier,
            self.certificate_identity.as_deref(),
            self.certificate_oidc_issuer.as_deref(),
        )
        .await?;

        let options = SbomOptions {
            policies: &policies,
            client,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            offline,
            state: &context.file_structure().state,
            no_cache: self.no_cache,
            predicate_type: self.predicate_type.clone(),
        };
        // The unwrap is load-bearing, not ceremony: `PackageError` omits
        // `#[source]` on its `kind`, so without re-rooting the chain on the
        // bare `VerifyError` every sbom failure classifies to 1 instead of its
        // own code. Pinned by `each_verify_kind_keeps_its_own_exit_code`.
        let report = context
            .manager()
            .sbom_one(&identifier, &self.platform, options)
            .await
            .map_err(package_sign_common::verify_error_into_anyhow)?;

        match destination {
            Some(destination) => {
                let attestation = single_match(&identifier, &report.attestations)?;
                write_predicate(&destination, attestation.attestation.predicate.get().as_bytes()).await?;
            }
            None => {
                let listing = self.listing(report.attestations, report.refused);
                context.api().report(&listing)?;
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Project a scan into the report DTO, summarizing under `--summary`.
    ///
    /// One unsummarizable document refuses that entry, not the listing
    /// (PKG-22). `--summary` augments a listing that works without it, so a
    /// single SPDX document among four CycloneDX ones must not cost the
    /// operator the other three — and a scan is over N independent items, where
    /// a bare `?` in the loop is the shape that reports 1 of N as 0 of N.
    /// The refusal lands in the vocabulary the report already carries, beside
    /// the candidates the verify pipeline itself refused.
    fn listing(&self, attestations: Vec<AttestationMatch>, refused: Vec<RefusedCandidate>) -> SbomListingReport {
        let mut entries = Vec::with_capacity(attestations.len());
        let mut refusals: Vec<RefusedEntry> = refused
            .into_iter()
            .map(|candidate| RefusedEntry {
                referrer_digest: candidate.referrer_digest,
                reason: candidate.reason.to_string(),
                reason_kind: candidate.reason.kind_detail(),
            })
            .collect();

        for candidate in attestations {
            let summary = match self.summary {
                false => None,
                true => match summarize(
                    &candidate.attestation.predicate_type,
                    candidate.attestation.predicate.get().as_bytes(),
                ) {
                    Ok(summary) => Some(summary),
                    Err(reason) => {
                        refusals.push(RefusedEntry {
                            referrer_digest: candidate.verify.referrer_digest.to_string(),
                            reason,
                            reason_kind: SUMMARY_FAILED,
                        });
                        continue;
                    }
                },
            };
            entries.push(SbomEntry {
                predicate_type: candidate.attestation.predicate_type,
                subject_digest: candidate.attestation.subject_digest.to_string(),
                referrer_digest: candidate.verify.referrer_digest.to_string(),
                certificate_identity: candidate.verify.certificate_identity,
                certificate_oidc_issuer: candidate.verify.certificate_oidc_issuer,
                signed_at: package_sign_common::iso8601(candidate.verify.signed_at),
                summary,
            });
        }
        SbomListingReport::new(entries, refusals)
    }
}

/// The parsed `--output` destination, if the flag was given.
///
/// A free function rather than a method: it reads one field and nothing else,
/// so it needs no receiver (ARCH-02) and its test needs no clap struct.
fn destination(output: Option<&Path>) -> Option<OutputDestination> {
    output.map(|path| {
        if path == Path::new("-") {
            OutputDestination::Stdout
        } else {
            OutputDestination::File(path.to_path_buf())
        }
    })
}

/// Refuse writing raw predicate bytes to a terminal.
///
/// `UsageError` (64), not a data error: the bytes are fine, the destination is
/// the problem, and the remedy is a different invocation — the same reasoning
/// that puts `ProvenanceVersionUnsupported` at 64.
fn refuse_tty_output(destination: &OutputDestination, stdout_is_terminal: bool) -> Result<(), CommandError> {
    if matches!(destination, OutputDestination::Stdout) && stdout_is_terminal {
        return Err(CommandError::new(
            "refusing to write raw predicate bytes to a terminal: the document is publisher-authored \
             and unsanitized; redirect to a file or a pipe",
            cli::ExitCode::UsageError,
        ));
    }
    Ok(())
}

/// The one attestation `--output` may write, or a refusal naming the rest.
///
/// Zero matches never reaches here — the library ends that scan as
/// `AttestationNotFound` (79). More than one is `MultipleAttestations` (65)
/// naming every referrer digest, because picking one would let the registry's
/// listing order decide which document a consumer reads.
///
/// The refusal carries **every** distinct predicate type in the match set, not
/// the first one: a package can carry a CycloneDX SBOM and an SPDX one, and a
/// message naming only whichever the registry listed first states something
/// untrue about the other candidate and hides the `--type` value that would
/// actually resolve the ambiguity. `BTreeSet` both dedupes and sorts, so the
/// message is stable across listing order (DATA-DET-01).
fn single_match<'a>(
    identifier: &oci::Identifier,
    attestations: &'a [AttestationMatch],
) -> anyhow::Result<&'a AttestationMatch> {
    match attestations {
        [only] => Ok(only),
        many => Err(VerifyError::new(
            identifier.clone(),
            VerifyErrorKind::MultipleAttestations {
                predicate_types: many
                    .iter()
                    .map(|candidate| candidate.attestation.predicate_type.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                referrer_digests: many
                    .iter()
                    .map(|candidate| candidate.verify.referrer_digest.to_string())
                    .collect(),
            },
        )
        .into()),
    }
}

/// Summarize one verified predicate, or return the refusal prose.
///
/// The reader parses CycloneDX 1.5-1.7 only, so anything else is an explicit
/// refusal naming the offending type — never a silently empty summary
/// (`adr_sbom_attestations.md` D-e). The refusal is a `String` and not a
/// classified error because it does not end the process: [`Self::listing`]
/// records it as a [`RefusedEntry`] and carries on, so there is no exit code
/// for it to carry. A `CommandError` here would pin a `DataError` (65) that
/// nothing can ever exit with, and tell every reader the opposite of what
/// `--summary` does.
fn summarize(predicate_type: &str, document: &[u8]) -> Result<SbomSummaryOut, String> {
    sbom::cyclonedx::summarize_cyclonedx(document)
        .map(SbomSummaryOut::from)
        .map_err(|error| {
            format!("cannot summarize the {predicate_type} predicate: {error}; drop --summary, or narrow with --type cyclonedx")
        })
}

/// Write the verified predicate bytes verbatim.
async fn write_predicate(destination: &OutputDestination, bytes: &[u8]) -> anyhow::Result<()> {
    match destination {
        OutputDestination::Stdout => write_stream(&mut tokio::io::stdout(), bytes).await?,
        OutputDestination::File(path) => tokio::fs::write(path, bytes).await?,
    }
    Ok(())
}

/// Writes the slice and nothing else.
///
/// No trailing newline, no framing, no re-encoding: D-e pins `--output -` as
/// byte-exact, and a pipe is the whole reason that destination exists, so
/// `shasum` on the stream must equal `shasum` on the file. Generic over the
/// sink so the byte-exactness is assertable without capturing process stdout.
async fn write_stream<W: tokio::io::AsyncWrite + Unpin>(sink: &mut W, bytes: &[u8]) -> anyhow::Result<()> {
    sink.write_all(bytes).await?;
    sink.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_envelope::render_error_envelope;
    use ocx_lib::Error as LibError;
    use ocx_lib::package_manager::error::{PackageError, PackageErrorKind};

    fn identifier() -> oci::Identifier {
        oci::Identifier::parse("registry.example/pkg:1.0").expect("parse identifier")
    }

    fn envelope(err: &anyhow::Error) -> serde_json::Value {
        let json = render_error_envelope("package sbom", err).expect("render envelope");
        serde_json::from_str(&json).expect("valid json")
    }

    /// The code the process would exit with, through the same authority
    /// `main.rs` uses. `render_error_envelope` classifies with the *library*
    /// classifier alone, which by construction cannot downcast a CLI-local
    /// [`CommandError`] — so an envelope assertion on one of those reads 1
    /// no matter which code the command chose.
    fn exit_code(err: &anyhow::Error) -> u8 {
        crate::app::classify_error(err.as_ref()) as u8
    }

    /// The `verify_error_into_anyhow` re-rooting, asserted so it **discriminates**:
    /// two kinds with different codes, through the identical wrapper.
    ///
    /// A single-kind test cannot tell a working unwrap from a broken one that
    /// happens to agree on one value. `PackageError` omits `#[source]` on its
    /// `kind` and `Error::Verify` is `#[error(transparent)]`, so a naive
    /// `anyhow::Error::new(package_error)` leaves the classifier unable to
    /// reach the `VerifyErrorKind` — and **every** sbom exit collapses to 1.
    /// Neuter the unwrap and both rows below go to 1, which is exactly what
    /// two distinct expectations make visible.
    #[test]
    fn each_verify_kind_keeps_its_own_exit_code_through_the_wrapper() {
        let cases = [
            (VerifyErrorKind::AttestationNotFound, 79, "attestation_not_found"),
            (
                VerifyErrorKind::MultipleSignatures { count: 2 },
                65,
                "multiple_signatures",
            ),
        ];
        for (kind, expected_code, expected_detail) in cases {
            let id = identifier();
            let package_error = PackageError::new(
                id.clone(),
                PackageErrorKind::Internal(LibError::Verify(Box::new(VerifyError::new(id, kind)))),
            );
            let parsed = envelope(&package_sign_common::verify_error_into_anyhow(package_error));
            assert_eq!(
                parsed["exit_code"], expected_code,
                "`{expected_detail}` must keep its own code, not collapse to 1",
            );
            assert_eq!(parsed["error"]["detail"], expected_detail);
            assert_eq!(
                parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0",
                "the identifier must survive the PackageError wrap",
            );
        }
    }

    // ── `--output` ──────────────────────────────────────────────────────────

    #[test]
    fn a_bare_dash_is_stdout_and_anything_else_is_a_file() {
        assert_eq!(destination(None), None);
        assert_eq!(destination(Some(Path::new("-"))), Some(OutputDestination::Stdout));
        assert_eq!(
            destination(Some(Path::new("./-"))),
            Some(OutputDestination::File(PathBuf::from("./-"))),
            "only a bare `-` means stdout; a path that merely ends in one is a file",
        );
        assert_eq!(
            destination(Some(Path::new("bom.json"))),
            Some(OutputDestination::File(PathBuf::from("bom.json"))),
        );
    }

    /// Both outcomes of the TTY gate, on inputs this test controls — a
    /// refusal-only assertion cannot tell a working gate from one that refuses
    /// unconditionally.
    #[test]
    fn each_refusal_carries_its_own_frozen_slug() {
        // PKG-25: `reason_kind` is what a script branches on, so it has to
        // track the variant rather than be a constant. Two different kinds
        // through the same projection is what rules a constant out — one row
        // would pass whatever the field were wired to.
        let refused = vec![
            RefusedCandidate {
                referrer_digest: "sha256:aa".to_string(),
                reason: VerifyErrorKind::IdentityMismatch,
            },
            RefusedCandidate {
                referrer_digest: "sha256:bb".to_string(),
                reason: VerifyErrorKind::BundleParseFailed,
            },
        ];

        let report = command(&[]).listing(Vec::new(), refused);
        let slugs: Vec<&str> = report.refused.iter().map(|entry| entry.reason_kind).collect();

        assert_eq!(slugs, ["identity_mismatch", "bundle_parse_failed"]);
        assert_eq!(
            report.refused[0].reason,
            VerifyErrorKind::IdentityMismatch.to_string(),
            "the prose stays beside the slug, not replaced by it",
        );
    }

    #[test]
    fn stdout_to_a_terminal_is_refused_and_every_other_combination_is_allowed() {
        let refusal = refuse_tty_output(&OutputDestination::Stdout, true).expect_err("a TTY must be refused");
        let message = refusal.to_string();
        assert!(
            message.contains("redirect to a file or a pipe"),
            "the refusal must name the remedy: {message}"
        );
        assert_eq!(
            exit_code(&anyhow::Error::new(refusal)),
            64,
            "the bytes are fine and the destination is not: that is a usage error",
        );

        refuse_tty_output(&OutputDestination::Stdout, false).expect("a pipe is byte-exact and allowed");
        refuse_tty_output(&OutputDestination::File(PathBuf::from("bom.json")), true)
            .expect("a file is unaffected by what stdout happens to be");
    }

    // ── `--summary` ─────────────────────────────────────────────────────────

    #[test]
    fn a_cyclonedx_document_summarizes() {
        let document = br#"{"bomFormat":"CycloneDX","specVersion":"1.6","components":[{"name":"zlib"}]}"#;
        let summary = summarize("https://cyclonedx.org/bom", document).expect("1.6 is in range");
        assert_eq!(summary.spec_version, "1.6");
        assert_eq!(summary.component_count, 1);
    }

    /// S-019: a non-CycloneDX or out-of-range predicate is an explicit refusal
    /// naming the type and the remedy, never a silently empty summary.
    #[test]
    fn a_non_cyclonedx_predicate_is_refused_naming_the_type_and_the_remedy() {
        let message = summarize("https://spdx.dev/Document", br#"{"spdxVersion":"SPDX-2.3"}"#)
            .expect_err("an SPDX document is not summarizable");
        assert!(
            message.contains("https://spdx.dev/Document"),
            "the refusal must name the offending predicate type: {message}"
        );
        assert!(
            message.contains("--type cyclonedx"),
            "the refusal must name the remedy: {message}"
        );
    }

    #[test]
    fn an_out_of_range_cyclonedx_version_is_refused_too() {
        let message = summarize(
            "https://cyclonedx.org/bom",
            br#"{"bomFormat":"CycloneDX","specVersion":"1.4"}"#,
        )
        .expect_err("1.4 is below the supported range");
        assert!(
            message.contains("1.4"),
            "the refusal must name the version it read: {message}"
        );
    }

    // ── `--output` ambiguity ────────────────────────────────────────────────

    /// A verified attestation, as `sbom_one` would hand it back.
    fn attestation_match(referrer_hex: &str, predicate_type: &str) -> AttestationMatch {
        attestation_match_carrying(referrer_hex, predicate_type, "{}")
    }

    /// The same, with a predicate document `--summary` will actually read.
    fn attestation_match_carrying(referrer_hex: &str, predicate_type: &str, predicate: &str) -> AttestationMatch {
        let digest = |hex: &str| {
            ocx_lib::oci::Digest::try_from(format!("sha256:{}", hex.repeat(64 / hex.len())).as_str())
                .expect("build test digest")
        };
        AttestationMatch {
            verify: ocx_lib::oci::verify::VerifyResult {
                subject_digest: digest("a"),
                referrer_digest: digest(referrer_hex),
                certificate_identity: "you@example.com".into(),
                certificate_oidc_issuer: "https://token.actions.githubusercontent.com".into(),
                signed_at: 1_755_597_600,
            },
            attestation: ocx_lib::oci::verify::VerifiedAttestation {
                predicate_type: predicate_type.into(),
                payload: b"{}".to_vec(),
                predicate: serde_json::value::RawValue::from_string(predicate.to_string()).expect("raw value"),
                subject_digest: digest("a"),
            },
        }
    }

    /// A parsed command, so the `--summary` gate under test is the one clap
    /// actually wires rather than a hand-set field.
    fn command(extra: &[&str]) -> PackageSbom {
        let mut argv = vec!["sbom", "--platform", "linux/amd64"];
        argv.extend_from_slice(extra);
        argv.push("registry.example/pkg:1.0");
        PackageSbom::try_parse_from(argv).expect("the fixture invocation parses")
    }

    const CYCLONEDX: &str = r#"{"bomFormat":"CycloneDX","specVersion":"1.6","components":[{"name":"zlib"}]}"#;
    const SPDX: &str = r#"{"spdxVersion":"SPDX-2.3"}"#;

    /// S-007: two real matches, so the refusal is asserted on the path a user
    /// reaches. The empty slice this once drove is structurally unreachable —
    /// the library ends a zero-match scan as `AttestationNotFound` — and it
    /// also could not carry a predicate type or a second digest, which are the
    /// two things the message has to name for `--type` to be actionable advice.
    #[test]
    fn more_than_one_match_refuses_with_the_digests_named() {
        let matches = [
            attestation_match("b", "https://cyclonedx.org/bom"),
            attestation_match("c", "https://cyclonedx.org/bom"),
        ];
        let error = single_match(&identifier(), &matches).expect_err("two matches are not one match");
        let parsed = envelope(&error);

        assert_eq!(parsed["exit_code"], 65);
        assert_eq!(parsed["error"]["detail"], "multiple_attestations");

        let message = parsed["error"]["message"].as_str().expect("message is a string");
        for hex in ["b".repeat(64), "c".repeat(64)] {
            assert!(
                message.contains(&hex),
                "both candidate digests must be named, or `--type` is unactionable advice: {message}"
            );
        }
        assert!(
            message.contains("https://cyclonedx.org/bom"),
            "the real predicate type must be named, not a default: {message}"
        );
        assert!(
            message.contains("--type cannot narrow further"),
            "one type across every match means --type is not the remedy, and saying so \
             is the difference between advice and a loop: {message}"
        );
    }

    /// UF-2: a mixed-type match set. The message must name **both** types and
    /// point at `--type`; naming only the first — which is what
    /// `many.first()` produced — tells the operator the SPDX candidate is a
    /// CycloneDX one, and withholds the one value that resolves the ambiguity.
    #[test]
    fn a_mixed_type_match_set_names_every_type_and_the_type_flag() {
        let matches = [
            attestation_match("b", "https://spdx.dev/Document"),
            attestation_match("c", "https://cyclonedx.org/bom"),
        ];
        let error = single_match(&identifier(), &matches).expect_err("two matches are not one match");
        let parsed = envelope(&error);

        assert_eq!(parsed["exit_code"], 65);
        assert_eq!(parsed["error"]["detail"], "multiple_attestations");

        let message = parsed["error"]["message"].as_str().expect("message is a string");
        for predicate_type in ["https://spdx.dev/Document", "https://cyclonedx.org/bom"] {
            assert!(
                message.contains(predicate_type),
                "every matched predicate type must be named, not just the first: {message}"
            );
        }
        assert!(
            message.contains("--type"),
            "the flag that disambiguates a mixed-type set must be named: {message}"
        );
    }

    /// The type list is sorted and deduplicated, so the same match set reads
    /// the same however the registry happened to order its referrers.
    #[test]
    fn the_named_types_are_deduplicated_and_ordered() {
        let matches = [
            attestation_match("d", "https://spdx.dev/Document"),
            attestation_match("b", "https://cyclonedx.org/bom"),
            attestation_match("c", "https://spdx.dev/Document"),
        ];
        let error = single_match(&identifier(), &matches).expect_err("three matches are not one match");
        let message = envelope(&error)["error"]["message"]
            .as_str()
            .expect("message is a string")
            .to_owned();

        assert_eq!(
            message.matches("https://spdx.dev/Document").count(),
            1,
            "a repeated type must be named once, not once per candidate: {message}"
        );
        let cyclonedx = message.find("https://cyclonedx.org/bom").expect("cyclonedx is named");
        let spdx = message.find("https://spdx.dev/Document").expect("spdx is named");
        assert!(
            cyclonedx < spdx,
            "the type list is sorted, so listing order cannot change the message: {message}"
        );
    }

    /// The Ok side of the same seam — without it, a `single_match` that refused
    /// unconditionally would pass the test above.
    #[test]
    fn exactly_one_match_is_returned() {
        let matches = [attestation_match("b", "https://cyclonedx.org/bom")];
        let only = single_match(&identifier(), &matches).expect("one match is the whole point");
        assert_eq!(
            only.verify.referrer_digest.to_string(),
            format!("sha256:{}", "b".repeat(64))
        );
    }

    // ── listing under `--summary` ───────────────────────────────────────────

    /// UF-3: one document `--summary` cannot read refuses that entry, never
    /// the listing (PKG-22).
    ///
    /// The library already refuses per candidate — `AttestationScan`'s own doc
    /// says failing closed would "hand a single malformed referrer the power
    /// to hide every valid attestation on the subject". A bare `?` in this
    /// loop handed it that power back one layer up, and it takes exactly one
    /// SPDX document beside a CycloneDX one to trigger.
    #[test]
    fn one_unsummarizable_document_refuses_that_entry_and_keeps_the_listing() {
        let listing = command(&["--summary"]).listing(
            vec![
                attestation_match_carrying("b", "https://cyclonedx.org/bom", CYCLONEDX),
                attestation_match_carrying("c", "https://spdx.dev/Document", SPDX),
            ],
            Vec::new(),
        );

        assert_eq!(
            listing.entries.len(),
            1,
            "the readable document must still be listed, not lost with the other one",
        );
        assert_eq!(listing.entries[0].referrer_digest, format!("sha256:{}", "b".repeat(64)));
        assert!(
            listing.entries[0].summary.is_some(),
            "the entry that did summarize must still carry its summary",
        );

        assert_eq!(listing.refused.len(), 1, "the unreadable document moves to refused");
        let refusal = &listing.refused[0];
        assert_eq!(refusal.referrer_digest, format!("sha256:{}", "c".repeat(64)));
        assert_eq!(
            refusal.reason_kind, "sbom_summary_failed",
            "a script branches on the slug, so it must be the summary one, not a verify slug",
        );
        assert!(
            refusal.reason.contains("https://spdx.dev/Document"),
            "the refusal prose must name the type it could not read: {}",
            refusal.reason,
        );

        assert_eq!(listing.summary.total, 2, "every candidate is still accounted for");
        assert_eq!(listing.summary.verified, 1);
        assert_eq!(listing.summary.refused, 1);
        assert_eq!(listing.summary.status, "partial_failure");
        assert_eq!(
            listing.summary.exit_code, 0,
            "a refusal beside a result is the reported path, which has exactly one code",
        );
    }

    /// The gate's other side: without `--summary` nothing is parsed, so a
    /// document no reader understands is an ordinary listing row.
    #[test]
    fn without_summary_an_unreadable_document_is_listed_like_any_other() {
        let listing = command(&[]).listing(
            vec![attestation_match_carrying("c", "https://spdx.dev/Document", SPDX)],
            Vec::new(),
        );

        assert_eq!(listing.entries.len(), 1);
        assert!(listing.entries[0].summary.is_none());
        assert!(
            listing.refused.is_empty(),
            "nothing was read, so nothing can be refused"
        );
        assert_eq!(listing.summary.status, "success");
    }

    /// A summary refusal joins the pipeline's own refusals rather than
    /// replacing them — both kinds are reported, and the slugs stay distinct.
    #[test]
    fn summary_refusals_travel_beside_the_pipelines_own() {
        let listing = command(&["--summary"]).listing(
            vec![attestation_match_carrying("c", "https://spdx.dev/Document", SPDX)],
            vec![RefusedCandidate {
                referrer_digest: format!("sha256:{}", "d".repeat(64)),
                reason: VerifyErrorKind::MultipleSignatures { count: 2 },
            }],
        );

        assert!(listing.entries.is_empty());
        assert_eq!(listing.summary.total, 2);
        let slugs: Vec<&str> = listing.refused.iter().map(|entry| entry.reason_kind).collect();
        assert_eq!(
            slugs,
            vec!["multiple_signatures", "sbom_summary_failed"],
            "the pipeline's refusals come first and keep their own slugs",
        );
    }

    // ── byte-exactness ──────────────────────────────────────────────────────

    /// D-e pins `--output` as the bytes the publisher signed. Both branches are
    /// asserted: a stream-only test cannot see a file branch that re-encodes,
    /// and a file-only test cannot see the trailing newline a pipe would carry.
    #[tokio::test]
    async fn both_destinations_emit_the_input_slice_verbatim() {
        // No trailing newline in the fixture, so an appended one is visible.
        let predicate = br#"{"bomFormat":"CycloneDX","specVersion":"1.6"}"#;

        let mut stream = Vec::new();
        write_stream(&mut stream, predicate).await.expect("write to stream");
        assert_eq!(
            stream, predicate,
            "the stream branch must not frame, pad or newline-terminate the predicate",
        );

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("bom.json");
        write_predicate(&OutputDestination::File(path.clone()), predicate)
            .await
            .expect("write to file");
        let written = tokio::fs::read(&path).await.expect("read back");
        assert_eq!(written, predicate, "the file branch must be byte-identical too");

        assert_eq!(stream, written, "and the two destinations must agree with each other");
    }
}
