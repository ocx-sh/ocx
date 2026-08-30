// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package sbom` — list, extract or summarize the verified SBOM
//! attestations a published package carries.
//!
//! The attestation twin of `ocx package verify`: same identity resolution, same
//! trust-root ladder, same pipeline — different arity.
//!
//! Two verification modes, resolved per invocation. **Demand**
//! is what an operator who has said who may sign gets: full crypto, and an
//! unsigned attachment is refused rather than listed. **Permissive** is what
//! everyone else gets: no cryptography runs at all, and every document — raw
//! attachment or bundle payload — is listed `verified: false`.
//!
//! Permissive is not a hole in SEC-32: every row it emits is labelled
//! unverified in both output formats, carries no signer identity, and the
//! effective mode is reported in the summary, so nothing registry-controlled is
//! ever presented as fact. What it replaces is worse — until this existed, a
//! consumer with no Sigstore setup could not read a published SBOM at all.
//!
//! Three output shapes, mutually exclusive by construction (clap
//! `conflicts_with`): the default listing, `--output PATH` writing one
//! predicate verbatim, and `--summary` parsing each CycloneDX document.
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
use ocx_lib::oci::verify::{
    AttestationMatch, RefusedCandidate, TrustRoot, UnverifiedSbom, VerificationMode, VerifyError, VerifyErrorKind,
};
use ocx_lib::package_manager::SbomOptions;
use ocx_lib::sbom;
use ocx_lib::trust::CompiledPolicy;

use crate::api::data::sanitize_for_terminal;
use crate::api::data::sbom::{ListingVerification, RefusedEntry, SbomEntry, SbomListingReport, SbomSummaryOut};
use crate::app::CommandError;
use crate::command::package_sign_common;
use crate::options;

/// List the SBOM attestations a published package carries, verified or not.
#[derive(Parser, Clone)]
pub struct PackageSbom {
    /// Narrow into one platform of an image index.
    ///
    /// Omit it to act on whatever the reference resolves to: an index is then
    /// the subject itself, which is where cosign puts a multi-platform tag's
    /// signature. Given against a reference that resolves to a single manifest,
    /// there is nothing to narrow and the command fails.
    #[clap(short = 'p', long = "platform", value_name = "PLATFORM")]
    platform: Option<oci::Platform>,

    /// Write the SBOM document to PATH ("-" for stdout).
    ///
    /// The bytes are exactly what was attached, never a re-serialization.
    /// Under --no-verify nothing was checked, so the document is written
    /// with a warning on stderr saying so. More than one candidate of the
    /// requested type is a refusal, not a choice: narrow with --type.
    /// Writing raw predicate bytes to a terminal is refused - redirect to a
    /// file or a pipe.
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
    /// without the other is an error. Not usable with `--key`: a key signature
    /// carries no certificate, so there is no SAN to match.
    #[clap(
        long = "certificate-identity",
        value_name = "IDENTITY",
        requires = "certificate_oidc_issuer",
        conflicts_with = "key"
    )]
    certificate_identity: Option<String>,

    /// Expected certificate OIDC issuer (exact match).
    ///
    /// Optional when a matching `[trust.policy]` supplies the issuer; used
    /// together with `--certificate-identity` to override any policy. Not
    /// usable with `--key`, which names a public key rather than an issuer.
    #[clap(
        long = "certificate-oidc-issuer",
        value_name = "URL",
        requires = "certificate_identity",
        conflicts_with = "key"
    )]
    certificate_oidc_issuer: Option<String>,

    /// Verify against a pinned public key instead of a Fulcio certificate.
    ///
    /// The key is a plain SPKI PEM — the public half only. No password is read
    /// and no decryption happens: `OCX_KEY_PASSWORD` belongs to signing.
    #[clap(flatten)]
    key: options::key::KeyOpt,

    /// Which cosign wire shape to accept.
    #[clap(flatten)]
    signature_format: options::signature_format::SignatureFormatOpt,

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

    #[clap(flatten)]
    verification: options::Verification,

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

        // D9's format pin, resolved and refused at the invocation boundary:
        // `--signature-format both` names two shapes, and a verification result
        // cannot say "either of these satisfied me", so it is a usage error (64)
        // rather than a silent pick — before any network request rather than
        // after one. The resolved pin then decides *discovery*: the shape it
        // does not name is never looked for.
        let signature_format = self.signature_format.pin().map_err(cli::UsageError::from)?;

        // Parsed before any request, so `--key awskms://alias/release` names its
        // unimplemented backend (exit 85) instead of being read as a filename
        // and reported as a missing file.
        let key = self
            .key
            .reference()
            .map_err(|error| VerifyError::new(identifier.clone(), VerifyErrorKind::from(error)))?;

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
        let (verification, policies) = self.mode(&context, &identifier, key.as_ref()).await?;

        // Neither is resolved under `Permissive`, and that is the gap fix, not
        // an optimization: `resolve_policies` refuses an invocation with no
        // identity source (exit 64), so calling it unconditionally locked every
        // consumer without a trust policy out of reading SBOMs entirely. The
        // trust root goes with it — a TUF fetch to serve a run that verifies
        // nothing is latency spent on material nothing will read.
        //
        // `TrustRoot::default()` carries no anchors and no CT-log key, so it
        // fails closed on contact: if this value ever reached the signed pass
        // through a later refactor, that pass refuses with `NoCtLogKey` rather
        // than verifying against nothing.
        let trust_root = match verification {
            VerificationMode::Permissive => TrustRoot::default(),
            VerificationMode::Demand => {
                let rekor_cache_key = ocx_lib::oci::verify::trust_cache::cache_key_for_rekor(&rekor_url);
                package_sign_common::resolve_trust_root(
                    &context,
                    &identifier,
                    &rekor_cache_key,
                    offline,
                    self.trusted_root.as_deref(),
                )
                .await?
            }
        };

        let options = SbomOptions {
            policies: &policies,
            client,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            offline,
            state: &context.file_structure().state,
            no_cache: self.no_cache,
            predicate_type: self.predicate_type.clone(),
            verification,
            signature_format,
        };
        // The unwrap is load-bearing, not ceremony: `PackageError` omits
        // `#[source]` on its `kind`, so without re-rooting the chain on the
        // bare `VerifyError` every sbom failure classifies to 1 instead of its
        // own code. Pinned by `each_verify_kind_keeps_its_own_exit_code`.
        let report = context
            .manager()
            .sbom_one(&identifier, self.platform.as_ref(), options)
            .await
            .map_err(package_sign_common::verify_error_into_anyhow)?;

        match destination {
            Some(destination) => {
                let selected = single_document(&identifier, &report.attestations, &report.unverified, report.refused)?;
                if let Selected::Unverified(sbom) = &selected {
                    // One line, on stderr, so `--output -` piped to a file is
                    // still byte-exact. Registry-sourced, so sanitized (CWE-150).
                    ocx_lib::log::warn!(
                        "SBOM is unverified: no signature over referrer {} was checked, \
                         so nothing vouches for what it says",
                        sanitize_for_terminal(&sbom.referrer_digest.to_string())
                    );
                }
                write_predicate(&destination, selected.document()).await?;
            }
            None => {
                let listing = self.listing(
                    verification,
                    report.attestations,
                    report.unverified,
                    report.refused,
                    &report.shadowed,
                );
                context.api().report(&listing)?;
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Resolve the verification mode, and the policies it will verify against.
    ///
    /// Three inputs, one answer:
    ///
    /// - `--no-verify` → permissive, and **nothing is resolved**. This is the
    ///   gap fix: [`package_sign_common::resolve_policies`] refuses an
    ///   invocation with no identity source (exit 64), so calling it here
    ///   regardless of mode is what locked every consumer without a trust
    ///   policy out of reading SBOMs at all.
    /// - `--verify` → demand, through the strict resolution, so demanding
    ///   verification with nothing to verify against is still the usage error
    ///   it has always been.
    /// - `--key` → demand against that one public key, short-circuiting every
    ///   keyless matcher. Combined with `--no-verify` it is a usage error, for
    ///   the reason the certificate flags already are.
    /// - neither → the invocation decides. Identity flags, or a
    ///   `[trust.policy]` covering the target, mean verification was asked
    ///   for. Neither means there is nothing to verify against, and refusing
    ///   would answer a question nobody asked.
    ///
    /// The empty policy set therefore has two readings, and the flag is what
    /// picks between them: under `--verify` it is "you named nothing to verify
    /// against" (64), and by default it is "no policy governs this package".
    /// One resolution, two readings — see
    /// [`package_sign_common::resolve_policies_lenient`].
    async fn mode(
        &self,
        context: &crate::app::Context,
        identifier: &oci::Identifier,
        key: Option<&ocx_lib::oci::sign::KeyRef>,
    ) -> anyhow::Result<(VerificationMode, Vec<CompiledPolicy>)> {
        let requested = self.verification.requested();
        if requested == Some(VerificationMode::Permissive) {
            // The refusal `--no-verify` already carries for the certificate
            // flags, extended to `--key` from here rather than from the frozen
            // option group. Naming a key while asking for no cryptography is
            // the same contradiction, and the alternative is worse than a
            // usage error: the key would be accepted and silently never used.
            if key.is_some() {
                return Err(cli::UsageError::new(
                    "--no-verify cannot be combined with --key: it names a key nothing would check",
                )
                .into());
            }
            return Ok((VerificationMode::Permissive, Vec::new()));
        }
        let policies = package_sign_common::resolve_policies_lenient(
            context,
            identifier,
            self.certificate_identity.as_deref(),
            self.certificate_oidc_issuer.as_deref(),
            key,
        )
        .await?;
        match resolve_mode(requested, policies.is_empty()) {
            Some(VerificationMode::Demand) => Ok((VerificationMode::Demand, policies)),
            Some(VerificationMode::Permissive) => Ok((VerificationMode::Permissive, Vec::new())),
            None => Err(VerifyError::new(identifier.clone(), VerifyErrorKind::NoIdentityProvided).into()),
        }
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
    fn listing(
        &self,
        verification: VerificationMode,
        attestations: Vec<AttestationMatch>,
        unverified: Vec<UnverifiedSbom>,
        refused: Vec<RefusedCandidate>,
        shadowed: &BTreeSet<oci::Digest>,
    ) -> SbomListingReport {
        let mut entries = Vec::with_capacity(attestations.len() + unverified.len());
        let mut refusals: Vec<RefusedEntry> = refused
            .into_iter()
            .map(|candidate| RefusedEntry {
                referrer_digest: candidate.referrer_digest,
                reason: candidate.reason.to_string(),
                reason_kind: candidate.reason.kind_detail(),
            })
            .collect();

        for candidate in attestations {
            let is_shadowed = shadowed.contains(&candidate.verify.referrer_digest);
            let referrer_digest = candidate.verify.referrer_digest.to_string();
            let summary = match self.summary_for(
                &candidate.attestation.predicate_type,
                candidate.attestation.predicate.get().as_bytes(),
                &referrer_digest,
            ) {
                Ok(summary) => summary,
                Err(refusal) => {
                    refusals.push(refusal);
                    continue;
                }
            };
            entries.push(SbomEntry {
                predicate_type: candidate.attestation.predicate_type,
                verified: true,
                shadowed: is_shadowed,
                subject_digest: candidate.attestation.subject_digest.to_string(),
                referrer_digest,
                certificate_identity: candidate.verify.certificate_identity,
                certificate_oidc_issuer: candidate.verify.certificate_oidc_issuer,
                signed_at: candidate.verify.signed_at.map(package_sign_common::iso8601),
                summary,
            });
        }

        // The unsigned half, through the same summarizer and the same refusal
        // channel: `--summary` reads a document, and whether anyone signed it
        // says nothing about whether it parses.
        for candidate in unverified {
            let is_shadowed = shadowed.contains(&candidate.referrer_digest);
            let referrer_digest = candidate.referrer_digest.to_string();
            let summary = match self.summary_for(&candidate.predicate_type, &candidate.document, &referrer_digest) {
                Ok(summary) => summary,
                Err(refusal) => {
                    refusals.push(refusal);
                    continue;
                }
            };
            entries.push(SbomEntry {
                predicate_type: candidate.predicate_type,
                verified: false,
                shadowed: is_shadowed,
                subject_digest: candidate.subject_digest.to_string(),
                referrer_digest,
                certificate_identity: None,
                certificate_oidc_issuer: None,
                signed_at: None,
                summary,
            });
        }
        let verification = match verification {
            VerificationMode::Demand => ListingVerification::Verified,
            VerificationMode::Permissive => ListingVerification::Unverified,
        };
        SbomListingReport::new(verification, entries, refusals)
    }

    /// The `--summary` cell for one document, or the refusal that replaces its
    /// whole listing row.
    ///
    /// `Ok(None)` is the no-`--summary` case, not a failure: the listing works
    /// without the flag, and only a document the flag could not read costs its
    /// entry (PKG-22).
    fn summary_for(
        &self,
        predicate_type: &str,
        document: &[u8],
        referrer_digest: &str,
    ) -> Result<Option<SbomSummaryOut>, RefusedEntry> {
        if !self.summary {
            return Ok(None);
        }
        summarize(predicate_type, document)
            .map(Some)
            .map_err(|reason| RefusedEntry {
                referrer_digest: referrer_digest.to_string(),
                reason,
                reason_kind: SUMMARY_FAILED,
            })
    }
}

/// The mode decision itself, over values: what the flags asked for, and
/// whether any identity source resolved. `None` is the usage error.
///
/// Split from [`PackageSbom::mode`] because that one reads a project file and
/// a config tier to answer the second question, and the decision over the
/// answer needs neither (ARCH-12). Every row of the matrix is then a test
/// that constructs nothing.
fn resolve_mode(requested: Option<VerificationMode>, policies_empty: bool) -> Option<VerificationMode> {
    match (requested, policies_empty) {
        // `--no-verify` never reaches here; the caller short-circuits it
        // before resolving anything, which is the point of the flag.
        (Some(VerificationMode::Permissive), _) => Some(VerificationMode::Permissive),
        // Verification demanded with nothing to verify against.
        (Some(VerificationMode::Demand), true) => None,
        (Some(VerificationMode::Demand), false) => Some(VerificationMode::Demand),
        // No flag: the invocation decides. An identity source means somebody
        // asked for verification; its absence means nobody did, and refusing
        // would answer a question nobody put.
        (None, true) => Some(VerificationMode::Permissive),
        (None, false) => Some(VerificationMode::Demand),
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

/// Which document `--output` resolved to, and what backs it.
///
/// A two-variant enum rather than a `(&[u8], bool)` pair so the warning cannot
/// be forgotten at a call site that already has the bytes: reading the document
/// out of an unverified match is a `match` the compiler makes visible.
#[derive(Debug)]
enum Selected<'a> {
    /// A document with a verified signature over it.
    Verified(&'a AttestationMatch),
    /// A document attached with no signature at all.
    Unverified(&'a UnverifiedSbom),
}

impl Selected<'_> {
    /// The bytes to write, verbatim in both cases.
    fn document(&self) -> &[u8] {
        match self {
            Self::Verified(candidate) => candidate.attestation.predicate.get().as_bytes(),
            Self::Unverified(candidate) => &candidate.document,
        }
    }
}

/// The one document `--output` may write, or a refusal naming the rest.
///
/// **A truncated scan is refused before anything is picked.** `--output` needs
/// exactly one candidate, and truncation is the state in which "exactly one"
/// cannot be established: a second SBOM the budget never reached is
/// indistinguishable from no second SBOM, so the ambiguity check below fails
/// open and the command writes one document silently, exit 0. A demanded scan
/// already fails closed on this inside the library (`finish_scan`); a
/// permissive one deliberately does not, because a *listing* survives
/// truncation and reports it as `partial_failure`. That is right for a listing
/// and wrong for a pick, and this is where the asymmetry is repaired. The check
/// lives here rather than at the call site so a pick cannot be made without it.
///
/// Zero of either kind never reaches here — the library ends that scan as
/// `AttestationNotFound` (79). More than one is `MultipleAttestations` (65)
/// naming every referrer digest, because picking one would let the registry's
/// listing order decide which document a consumer reads.
///
/// **A verified document wins outright**, and the unverified set is not even
/// looked at. That precedence is defensive rather than reachable: the two
/// lists are mode-exclusive by construction — a demanded scan refuses unsigned
/// attachments instead of listing them, and a permissive one verifies nothing
/// — so exactly one of them is ever non-empty. Kept as the fail-safe ordering
/// anyway, because if that ever stops holding, the answer that must not depend
/// on listing order is which trust class `--output` writes. Ambiguity is
/// judged **within** a trust class and never across it.
///
/// The refusal carries **every** distinct predicate type in the colliding set,
/// not the first one: a package can carry a CycloneDX SBOM and an SPDX one, and
/// a message naming only whichever the registry listed first states something
/// untrue about the other candidate and hides the `--type` value that would
/// actually resolve the ambiguity. `BTreeSet` both dedupes and sorts, so the
/// message is stable across listing order (DATA-DET-01).
fn single_document<'a>(
    identifier: &oci::Identifier,
    attestations: &'a [AttestationMatch],
    unverified: &'a [UnverifiedSbom],
    refused: Vec<RefusedCandidate>,
) -> anyhow::Result<Selected<'a>> {
    if let Some(reason) = truncation_refusal(refused) {
        return Err(VerifyError::new(identifier.clone(), reason).into());
    }
    match attestations {
        [only] => return Ok(Selected::Verified(only)),
        [] => {}
        many => {
            return Err(ambiguous(
                identifier,
                many.iter()
                    .map(|candidate| {
                        (
                            candidate.attestation.predicate_type.clone(),
                            candidate.verify.referrer_digest.to_string(),
                        )
                    })
                    .collect(),
            ));
        }
    }
    match unverified {
        [only] => Ok(Selected::Unverified(only)),
        // Unreachable: a scan with nothing of either kind ends as
        // `AttestationNotFound` inside the library. Returned rather than
        // asserted — a panic here would be the CLI crashing on a library
        // contract change instead of reporting one.
        [] => Err(VerifyError::new(identifier.clone(), VerifyErrorKind::AttestationNotFound).into()),
        many => Ok(Err(ambiguous(
            identifier,
            many.iter()
                .map(|candidate| (candidate.predicate_type.clone(), candidate.referrer_digest.to_string()))
                .collect(),
        ))?),
    }
}

/// The truncation refusal among a scan's refused candidates, if it carries one.
///
/// The three kinds are the whole of what [`ScanBudget`] can stop on — a
/// candidate cap, a byte budget, a listing cap — and they are the only refusals
/// that say something about the candidates that were *not* examined. Every
/// other refusal is about one candidate that was.
///
/// Matched exhaustively rather than by a catch-all: a new budget stop must
/// either be added here or be a deliberate decision not to refuse a pick.
///
/// [`ScanBudget`]: ocx_lib::oci::verify
fn truncation_refusal(refused: Vec<RefusedCandidate>) -> Option<VerifyErrorKind> {
    refused.into_iter().map(|candidate| candidate.reason).find(|reason| {
        matches!(
            reason,
            VerifyErrorKind::TooManyAttestations { .. }
                | VerifyErrorKind::AttestationBudgetExhausted { .. }
                | VerifyErrorKind::CandidateLimitExhausted { .. }
        )
    })
}

/// The `MultipleAttestations` refusal over one trust class's colliding set.
fn ambiguous(identifier: &oci::Identifier, candidates: Vec<(String, String)>) -> anyhow::Error {
    VerifyError::new(
        identifier.clone(),
        VerifyErrorKind::MultipleAttestations {
            predicate_types: candidates
                .iter()
                .map(|(predicate_type, _)| predicate_type.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            referrer_digests: candidates.into_iter().map(|(_, digest)| digest).collect(),
        },
    )
    .into()
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
        OutputDestination::File(path) => tokio::fs::write(path, bytes)
            .await
            .map_err(|error| ocx_lib::error::file_error(path, error))?,
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

        let report = command(&[]).listing(
            VerificationMode::Demand,
            Vec::new(),
            Vec::new(),
            refused,
            &BTreeSet::new(),
        );
        let slugs: Vec<&str> = report.refused.iter().map(|entry| entry.reason_kind).collect();

        assert_eq!(slugs, ["identity_mismatch", "bundle_parse_failed"]);
        assert_eq!(
            report.refused[0].reason,
            VerifyErrorKind::IdentityMismatch.to_string(),
            "the prose stays beside the slug, not replaced by it",
        );
    }

    // ── `--output` across the two trust classes ─────────────────────────────

    /// A verified document wins outright, and the unverified set is not even
    /// consulted. A publisher who signed an SBOM and left an unsigned one
    /// beside it has said which one they stand behind — and a registry that
    /// adds an unsigned referrer must not be able to turn a working `--output`
    /// into an ambiguity refusal.
    #[test]
    fn a_verified_document_wins_over_an_unverified_one() {
        let verified = [attestation_match_carrying("b", "https://cyclonedx.org/bom", CYCLONEDX)];
        let unverified = [unverified_sbom("c", "https://cyclonedx.org/bom", SPDX)];

        let selected =
            single_document(&identifier(), &verified, &unverified, Vec::new()).expect("the signed document wins");
        let Selected::Verified(only) = selected else {
            panic!("a verified document must outrank an unverified one");
        };
        assert_eq!(
            only.attestation.predicate.get(),
            CYCLONEDX,
            "the bytes written must be the signed ones",
        );
    }

    /// With nothing verified, the single unverified document is written — and
    /// the caller learns it is unverified from the variant, which is what makes
    /// the warning impossible to forget at the call site.
    #[test]
    fn a_lone_unverified_document_is_selected_and_marked_as_such() {
        let unverified = [unverified_sbom("c", "https://cyclonedx.org/bom", CYCLONEDX)];

        let selected =
            single_document(&identifier(), &[], &unverified, Vec::new()).expect("one document is one document");
        let Selected::Unverified(only) = selected else {
            panic!("with nothing verified the unsigned document is the answer");
        };
        assert_eq!(only.document, CYCLONEDX.as_bytes());
        assert_eq!(selected.document(), CYCLONEDX.as_bytes(), "written verbatim");
    }

    /// Ambiguity is judged **within** a trust class, never across it. Two
    /// verified documents collide; two unverified ones collide; one of each
    /// does not — which is the row `a_verified_document_wins_over_an_unverified_one`
    /// pins, and this is its complement.
    #[test]
    fn ambiguity_is_per_trust_class() {
        let two_unverified = [
            unverified_sbom("b", "https://cyclonedx.org/bom", CYCLONEDX),
            unverified_sbom("c", "https://spdx.dev/Document", SPDX),
        ];
        let error = single_document(&identifier(), &[], &two_unverified, Vec::new())
            .expect_err("two unverified documents are not one document");
        let parsed = envelope(&error);

        assert_eq!(parsed["exit_code"], 65);
        assert_eq!(parsed["error"]["detail"], "multiple_attestations");
        let message = parsed["error"]["message"].as_str().expect("message is a string");
        for named in [
            "https://cyclonedx.org/bom",
            "https://spdx.dev/Document",
            &"b".repeat(64),
            &"c".repeat(64),
        ] {
            assert!(
                message.contains(named),
                "the refusal must name every colliding candidate: {message}"
            );
        }

        // And the cross-class pair does NOT collide, so the rule is genuinely
        // per-class rather than "any two documents refuse".
        let verified = [attestation_match_carrying("b", "https://cyclonedx.org/bom", CYCLONEDX)];
        let unverified = [unverified_sbom("c", "https://cyclonedx.org/bom", SPDX)];
        assert!(
            single_document(&identifier(), &verified, &unverified, Vec::new()).is_ok(),
            "one verified and one unverified document is not an ambiguity",
        );
    }

    /// A truncated scan refuses to pick, even when it happens to hold exactly
    /// one document.
    ///
    /// This is the whole of W1: the one candidate here is real, and picking it
    /// would look correct. What makes it wrong is that the budget stopped
    /// before the listing was exhausted, so "exactly one" was never
    /// established — a second SBOM behind the cap is indistinguishable from no
    /// second SBOM, and the ambiguity check fails open rather than closed.
    ///
    /// Asserted on the exit code and the slug rather than the message, because
    /// those are the contract a script branches on.
    #[test]
    fn a_truncated_scan_refuses_to_pick_a_document() {
        let unverified = [unverified_sbom("c", "https://cyclonedx.org/bom", CYCLONEDX)];
        let refused = vec![RefusedCandidate {
            referrer_digest: "d".repeat(64),
            reason: VerifyErrorKind::TooManyAttestations { limit: 32 },
        }];

        let error = single_document(&identifier(), &[], &unverified, refused)
            .expect_err("a partial candidate set cannot answer which document");
        let parsed = envelope(&error);

        assert_eq!(parsed["exit_code"], 65);
        assert_eq!(parsed["error"]["detail"], "too_many_attestations");
    }

    /// Every budget stop refuses, not just the candidate cap — the three are
    /// one condition reached three ways, and a guard covering one of them is
    /// the same bug for the other two.
    #[test]
    fn every_budget_stop_refuses_a_pick() {
        for reason in [
            VerifyErrorKind::TooManyAttestations { limit: 32 },
            VerifyErrorKind::AttestationBudgetExhausted { limit: 65_536 },
            VerifyErrorKind::CandidateLimitExhausted { unexamined: 4 },
        ] {
            let unverified = [unverified_sbom("c", "https://cyclonedx.org/bom", CYCLONEDX)];
            let refused = vec![RefusedCandidate {
                referrer_digest: "d".repeat(64),
                reason,
            }];
            assert!(
                single_document(&identifier(), &[], &unverified, refused).is_err(),
                "a budget stop is a budget stop however it was reached",
            );
        }
    }

    /// An untruncated scan still serves, and a per-candidate refusal is not a
    /// truncation.
    ///
    /// The complement that keeps the guard from being "refuse whenever anything
    /// was refused": one malformed referrer beside one good document is exactly
    /// the case the per-candidate independence exists to keep working, and
    /// refusing it would hand a registry a denial of service for the price of
    /// one junk attachment.
    #[test]
    fn a_refusal_that_is_not_a_truncation_still_serves_the_document() {
        let unverified = [unverified_sbom("c", "https://cyclonedx.org/bom", CYCLONEDX)];
        let refused = vec![RefusedCandidate {
            referrer_digest: "d".repeat(64),
            reason: VerifyErrorKind::BundleParseFailed,
        }];

        let selected = single_document(&identifier(), &[], &unverified, refused)
            .expect("one unreadable sibling does not make the readable document ambiguous");
        assert_eq!(selected.document(), CYCLONEDX.as_bytes());
    }

    // ── the listing across the two trust classes ────────────────────────────

    /// Both classes reach the listing, each labelled, and `--summary` reads an
    /// unsigned document exactly as it reads a signed one: whether anyone
    /// signed it says nothing about whether it parses.
    ///
    /// Two listings, not one mixed listing: a scan returns verified matches or
    /// unverified documents and never both, because the mode decides which
    /// pass runs. What is shared is the projection under test — the same
    /// summarizer and the same refusal channel serve both.
    #[test]
    fn the_listing_carries_both_trust_classes_and_summarizes_either() {
        let demanded = command(&["--summary"]).listing(
            VerificationMode::Demand,
            vec![attestation_match_carrying("b", "https://cyclonedx.org/bom", CYCLONEDX)],
            Vec::new(),
            Vec::new(),
            &BTreeSet::new(),
        );
        assert_eq!(demanded.summary.verified, 1);
        assert_eq!(demanded.summary.unverified, 0);
        assert_eq!(
            demanded.summary.verification,
            ListingVerification::Verified,
            "a demanded listing must say so, so a script can read the rows correctly",
        );
        let signed = &demanded.entries[0];
        assert!(signed.verified);
        assert_eq!(signed.certificate_identity.as_deref(), Some("you@example.com"));
        assert!(signed.summary.is_some());

        let permissive = command(&["--summary"]).listing(
            VerificationMode::Permissive,
            Vec::new(),
            vec![unverified_sbom("c", "https://cyclonedx.org/bom", CYCLONEDX)],
            Vec::new(),
            &BTreeSet::new(),
        );
        assert_eq!(permissive.summary.verified, 0);
        assert_eq!(permissive.summary.unverified, 1);
        assert_eq!(
            permissive.summary.verification,
            ListingVerification::Unverified,
            "an unverified listing must say so: the rows look the same either way",
        );
        let unverified = &permissive.entries[0];
        assert!(!unverified.verified, "an unverified entry must be labelled as such");
        assert_eq!(
            unverified.certificate_identity, None,
            "an unverified entry has no checked certificate to name",
        );
        assert_eq!(unverified.signed_at, None);
        assert!(
            unverified.summary.is_some(),
            "--summary reads an unverified CycloneDX document like any other",
        );
    }

    /// The whole mode matrix, in one place, over the two inputs that decide
    /// it: what the flags asked for, and whether any identity source resolved.
    ///
    /// The `--verify`-with-nothing-to-verify-against row is the one that must
    /// stay an error. Every other row that resolves to permissive would be
    /// indistinguishable from it if that one silently degraded, and an
    /// operator who typed `--verify` would get an unverified listing.
    #[test]
    fn the_mode_matrix_resolves_every_combination() {
        use VerificationMode::{Demand, Permissive};

        // No flag: the identity sources decide.
        assert_eq!(resolve_mode(None, true), Some(Permissive));
        assert_eq!(resolve_mode(None, false), Some(Demand));
        // --verify: demanded, and an empty policy set is the usage error.
        assert_eq!(resolve_mode(Some(Demand), false), Some(Demand));
        assert_eq!(
            resolve_mode(Some(Demand), true),
            None,
            "--verify with no identity source must not degrade to permissive",
        );
        // --no-verify never reaches this function, but if it ever did it must
        // not be turned into a demand by a policy the operator refused.
        assert_eq!(resolve_mode(Some(Permissive), false), Some(Permissive));
        assert_eq!(resolve_mode(Some(Permissive), true), Some(Permissive));
    }

    /// The two flags name their modes, and neither names one.
    ///
    /// Asserted through the real command's argv so the flatten is covered:
    /// a `#[clap(flatten)]` that silently stopped wiring the pair would leave
    /// [`resolve_mode`] correct and the command permanently on its default.
    #[test]
    fn the_flags_reach_the_command_through_the_flatten() {
        assert_eq!(command(&[]).verification.requested(), None);
        assert_eq!(
            command(&["--verify"]).verification.requested(),
            Some(VerificationMode::Demand),
        );
        assert_eq!(
            command(&["--no-verify"]).verification.requested(),
            Some(VerificationMode::Permissive),
        );
    }

    /// `--no-verify` with an identity is contradictory, and clap refuses it
    /// (exit 64) rather than silently ignoring one of the two.
    #[test]
    fn no_verify_with_a_certificate_identity_is_a_usage_error() {
        let parsed = PackageSbom::try_parse_from([
            "sbom",
            "-p",
            "linux/amd64",
            "--no-verify",
            "--certificate-identity",
            "you@example.com",
            "--certificate-oidc-issuer",
            "https://example.com",
            "ocx.sh/acme/tool:1.0",
        ]);
        assert!(parsed.is_err(), "an identity flag with --no-verify must not parse");
    }

    /// The two verification flags last-win through the real command, both
    /// orders — the flatten must carry the `overrides_with` pair, not just the
    /// flags.
    #[test]
    fn verify_and_no_verify_last_win() {
        let permissive = command(&["--verify", "--no-verify"]);
        assert_eq!(
            permissive.verification.requested(),
            Some(VerificationMode::Permissive),
            "--no-verify wins when last",
        );
        let demand = command(&["--no-verify", "--verify"]);
        assert_eq!(
            demand.verification.requested(),
            Some(VerificationMode::Demand),
            "--verify wins when last",
        );
    }

    /// The refusal channel is shared too: an unsigned document `--summary`
    /// cannot read costs its own entry and not the listing (PKG-22).
    #[test]
    fn an_unsummarizable_unsigned_document_refuses_only_its_own_entry() {
        let listing = command(&["--summary"]).listing(
            VerificationMode::Permissive,
            Vec::new(),
            vec![
                unverified_sbom("b", "https://cyclonedx.org/bom", CYCLONEDX),
                unverified_sbom("c", "https://spdx.dev/Document", SPDX),
            ],
            Vec::new(),
            &BTreeSet::new(),
        );

        assert_eq!(listing.entries.len(), 1, "the readable document is still listed");
        assert_eq!(listing.entries[0].referrer_digest, format!("sha256:{}", "b".repeat(64)));
        assert_eq!(listing.refused.len(), 1);
        assert_eq!(listing.refused[0].reason_kind, "sbom_summary_failed");
        assert_eq!(listing.summary.unverified, 1);
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
                key_backend: ocx_lib::oci::sign::KeyBackendKind::Keyless,
                certificate_identity: Some("you@example.com".into()),
                certificate_oidc_issuer: Some("https://token.actions.githubusercontent.com".into()),
                signed_at: Some(1_755_597_600),
                signature_format: ocx_lib::oci::sign::SignatureFormat::Bundle,
                discovery_method: ocx_lib::oci::verify::DiscoveryMethod::ReferrersApi,
                rekor_log_index: None,
            },
            attestation: ocx_lib::oci::verify::VerifiedAttestation {
                predicate_type: predicate_type.into(),
                payload: b"{}".to_vec(),
                predicate: serde_json::value::RawValue::from_string(predicate.to_string()).expect("raw value"),
                subject_digest: digest("a"),
            },
        }
    }

    /// An SBOM attached with nothing behind it.
    fn unverified_sbom(referrer_hex: &str, predicate_type: &str, document: &str) -> UnverifiedSbom {
        let digest = |hex: &str| {
            ocx_lib::oci::Digest::try_from(format!("sha256:{}", hex.repeat(64 / hex.len())).as_str())
                .expect("build test digest")
        };
        UnverifiedSbom {
            referrer_digest: digest(referrer_hex),
            subject_digest: digest("a"),
            predicate_type: predicate_type.into(),
            document: document.as_bytes().to_vec(),
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
        let error =
            single_document(&identifier(), &matches, &[], Vec::new()).expect_err("two matches are not one match");
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
        let error =
            single_document(&identifier(), &matches, &[], Vec::new()).expect_err("two matches are not one match");
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
        let error =
            single_document(&identifier(), &matches, &[], Vec::new()).expect_err("three matches are not one match");
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
        let selected = single_document(&identifier(), &matches, &[], Vec::new()).expect("one match is the whole point");
        let Selected::Verified(only) = selected else {
            panic!("a verified match must select as verified");
        };
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
            VerificationMode::Demand,
            vec![
                attestation_match_carrying("b", "https://cyclonedx.org/bom", CYCLONEDX),
                attestation_match_carrying("c", "https://spdx.dev/Document", SPDX),
            ],
            Vec::new(),
            Vec::new(),
            &BTreeSet::new(),
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
            VerificationMode::Demand,
            vec![attestation_match_carrying("c", "https://spdx.dev/Document", SPDX)],
            Vec::new(),
            Vec::new(),
            &BTreeSet::new(),
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
            VerificationMode::Demand,
            vec![attestation_match_carrying("c", "https://spdx.dev/Document", SPDX)],
            Vec::new(),
            vec![RefusedCandidate {
                referrer_digest: format!("sha256:{}", "d".repeat(64)),
                reason: VerifyErrorKind::MultipleSignatures { count: 2 },
            }],
            &BTreeSet::new(),
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

    /// **C-011.** The library's shadow set reaches the entry it names, on both
    /// trust classes, and marks nothing else.
    ///
    /// The set is keyed on the referrer manifest digest, which is where a
    /// transposition would land: a lookup against the *subject* digest instead
    /// would mark every row on one subject, and a lookup against the wrong
    /// candidate's digest would mark the preferred document.
    #[test]
    fn the_shadow_set_marks_the_document_it_names_and_no_other() {
        let superseded = attestation_match_carrying("b", "https://cyclonedx.org/bom", CYCLONEDX);
        let shadowed = BTreeSet::from([superseded.verify.referrer_digest.clone()]);

        let listing = command(&[]).listing(
            VerificationMode::Demand,
            vec![
                superseded,
                attestation_match_carrying("c", "https://spdx.dev/Document", SPDX),
            ],
            Vec::new(),
            Vec::new(),
            &shadowed,
        );
        let marked: Vec<bool> = listing.entries.iter().map(|entry| entry.shadowed).collect();
        assert_eq!(
            marked,
            vec![true, false],
            "only the named referrer is marked; a subject-keyed lookup would mark both",
        );

        let unverified = unverified_sbom("b", "https://cyclonedx.org/bom", CYCLONEDX);
        let shadowed = BTreeSet::from([unverified.referrer_digest.clone()]);
        let permissive = command(&[]).listing(
            VerificationMode::Permissive,
            Vec::new(),
            vec![unverified, unverified_sbom("c", "https://spdx.dev/Document", SPDX)],
            Vec::new(),
            &shadowed,
        );
        assert_eq!(
            permissive
                .entries
                .iter()
                .map(|entry| entry.shadowed)
                .collect::<Vec<bool>>(),
            vec![true, false],
            "the unsigned half reads the same set; marking only the verified half would leave \
             --no-verify rendering both copies",
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
