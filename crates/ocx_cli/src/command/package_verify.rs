// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package verify` — Sigstore verification of a package's signature referrer. State machine:
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).
//!
//! No default identity or issuer: the pair comes from the flags or a matching `[[trust.policy]]`.
//! `--offline` scopes to the Sigstore trust services only, and without cached or supplied trust
//! material it fails rather than skips verification.

use std::process::ExitCode;

use clap::Parser;

use ocx_package_manager::VerifyOptions;
use ocx_sign::attest::predicate::PredicateType;
use ocx_sign::verify::{VerifyContentMode, VerifyError, VerifyErrorKind};

use crate::api::data::verification::{SignatureEntry, VerificationReport};
use crate::command::package_sign_common;
use crate::options;

#[derive(Parser, Clone)]
pub struct PackageVerify {
    /// Narrow into one platform of an image index.
    ///
    /// Omit it to act on whatever the reference resolves to: an index is then
    /// the subject itself, which is where cosign puts a multi-platform tag's
    /// signature. Given against a reference that resolves to a single manifest,
    /// there is nothing to narrow and the command fails.
    #[clap(short = 'p', long = "platform", value_name = "PLATFORM")]
    platform: Option<ocx_oci::Platform>,

    /// Expected certificate SAN (exact match).
    ///
    /// Optional when a `[trust.policy]` whose scope covers the target supplies
    /// the identity; when given, this flag and `--certificate-oidc-issuer`
    /// override any policy. The two flags are used together; supplying one
    /// without the other is an error. Not usable with `--key`: a key signature
    /// carries no certificate, so there is no SAN to match.
    ///
    /// Example: `you@example.com`, `https://github.com/org/repo/.github/workflows/build.yml@refs/heads/main`.
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
    ///
    /// Example: `https://github.com/login/oauth`, `https://token.actions.githubusercontent.com`.
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

    // `Option`, not a clap default, or `[trust.sigstore].rekor_url` can never apply.
    /// Rekor transparency-log endpoint
    ///
    /// Defaults to [trust.sigstore].rekor_url, else public Rekor.
    #[clap(long = "rekor-url", value_name = "URL")]
    rekor_url: Option<String>,

    /// Verify a signed in-toto attestation instead of an artifact signature.
    ///
    /// Same trust material and same identity resolution; a different kind of
    /// signed content. Use `ocx package sbom` to list or extract what an
    /// artifact carries.
    #[clap(long = "attestation")]
    attestation: bool,

    /// Restrict to one predicate type (for example cyclonedx or spdx).
    ///
    /// Narrowing is by the signed payload, never by a referrer annotation.
    #[clap(long = "type", value_name = "TYPE", requires = "attestation")]
    predicate_type: Option<PredicateType>,

    /// Accept a keyless cosign sidecar that carries no transparency-log entry.
    #[arg(long_help = "\
        Accept a keyless cosign sidecar that carries no transparency-log entry.\n\n\
        A keyless `sha256-<hex>.sig` or `sha256-<hex>.att` proves nothing about *when* it was \
        signed unless its layer carries a `dev.sigstore.cosign/bundle` annotation: the Fulcio \
        certificate it names lived about ten minutes, so without an entry a long-expired \
        certificate is indistinguishable from a live one. Verify refuses both shapes (exit 65). \
        Pass this in air-gapped CI, where the entry could not be fetched or was never written, and \
        you accept a signature nothing timestamps. Inert everywhere else: a bundle's transparency \
        evidence stays mandatory under keyless and optional under `--key`.")]
    #[clap(long = "allow-unlogged-signature")]
    allow_unlogged_signature: bool,

    /// Bypass the referrers-capability cache for this invocation.
    #[clap(long = "no-cache")]
    no_cache: bool,

    /// Trust-root override: a Sigstore trusted-root JSON (or a directory holding
    /// trusted_root.json), named by a bare path or a file:// one.
    ///
    /// Supplies the Fulcio CA, the CT-log key and the pinned Rekor public key
    /// for air-gapped verification against a local trust-root mirror. No TUF
    /// network fetch is performed. Takes precedence over the
    /// OCX_SIGSTORE_TRUSTED_ROOT env var and over [trust.sigstore] in
    /// config.toml. See
    /// https://ocx.sh/docs/in-depth/self-hosted-sigstore
    #[clap(long = "sigstore-trusted-root", value_name = "PATH")]
    trusted_root: Option<std::path::PathBuf>,

    /// Package identifier to verify (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl PackageVerify {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // `both` is a usage error (64): a verdict cannot report "either shape satisfied me".
        let signature_format = self.signature_format.pin().map_err(crate::error::UsageError::from)?;

        // Parsed up front, or `--key awskms://…` fails later as a missing file instead of exit 85.
        let key = self
            .key
            .reference()
            .map_err(|error| VerifyError::new(identifier.clone(), VerifyErrorKind::from(error)))?;

        // Also the SSRF guard (CWE-918): the endpoint is validated before any client dials it.
        let rekor_url = package_sign_common::resolve_rekor_endpoint(
            context.config_trust_sigstore(),
            &identifier,
            self.rekor_url.as_deref(),
        )?;

        // `verify_client`, not the offline-gated client: verify reads the registry under `--offline` too.
        let client = context.verify_client();
        let offline = context.is_offline();

        let rekor_cache_key = ocx_sign::verify::trust_cache::cache_key_for_rekor(&rekor_url);
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
            key.as_ref(),
        )
        .await?;

        let options = VerifyOptions {
            policies: &policies,
            client,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            offline,
            state: &context.file_structure().state,
            no_cache: self.no_cache,
            content: self.content_mode(),
            signature_format,
            allow_unlogged_signature: self.allow_unlogged_signature,
            // `signatures[]` reports every signature, so the scan must not stop at the first pass.
            report_all: true,
        };
        let verified = context
            .manager()
            .verify_one(&identifier, self.platform.as_ref(), options)
            .await
            .map_err(package_sign_common::verify_error_into_anyhow)?
            .signatures;

        let signatures: Vec<SignatureEntry> = verified.iter().map(Self::signature_entry).collect();
        // `VerifyPipeline::run` puts the passing verdict first, so `signatures[0]` matches the flat fields.
        let Some(result) = verified.into_iter().next() else {
            unreachable!("a successful verify returns at least one signature");
        };

        // Empty under `--key`: the flat fields are frozen as `String`; `signatures[]` keeps the typed absence.
        let mut report = VerificationReport::new(
            result.subject_digest,
            result.referrer_digest,
            result.certificate_identity.unwrap_or_default(),
            result.certificate_oidc_issuer.unwrap_or_default(),
            result.signed_at.map(package_sign_common::iso8601).unwrap_or_default(),
        );
        report.signatures = signatures;
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }

    /// Projects one verified candidate onto the frozen `signatures[]` row; never rendered in
    /// plain text (CWE-150, see `SignatureEntry`).
    fn signature_entry(result: &ocx_sign::verify::VerifyResult) -> SignatureEntry {
        SignatureEntry {
            signature_format: result.signature_format,
            discovery_method: result.discovery_method,
            key_backend: result.key_backend,
            referrer_digest: result.referrer_digest.clone(),
            certificate_identity: result.certificate_identity.clone(),
            certificate_oidc_issuer: result.certificate_oidc_issuer.clone(),
            signed_at: result.signed_at.map(package_sign_common::iso8601),
            rekor_log_index: result.rekor_log_index,
        }
    }

    fn content_mode(&self) -> VerifyContentMode {
        if self.attestation {
            VerifyContentMode::Attestation {
                predicate_type: self.predicate_type.clone(),
            }
        } else {
            VerifyContentMode::Signature
        }
    }
}
#[cfg(test)]
mod tests {
    /// The `--attestation` / `--type` wiring, asserted through the parser so a
    /// revert is visible. Both reverts the review named are covered: hardcoding
    /// `Signature` reds rows 2 and 3, and hardcoding `predicate_type: None`
    /// reds row 3 alone — which is why the table carries a narrowed row rather
    /// than stopping at "attestation mode is reachable".
    #[test]
    fn the_content_mode_follows_the_flags() {
        use ocx_sign::attest::predicate::PredicateType;

        let cases: [(&[&str], VerifyContentMode); 3] = [
            (&[], VerifyContentMode::Signature),
            (
                &["--attestation"],
                VerifyContentMode::Attestation { predicate_type: None },
            ),
            (
                &["--attestation", "--type", "cyclonedx"],
                VerifyContentMode::Attestation {
                    predicate_type: Some(PredicateType::CycloneDx),
                },
            ),
        ];

        for (flags, expected) in cases {
            let mut argv = vec!["verify", "-p", "linux/amd64"];
            argv.extend_from_slice(flags);
            argv.push("registry.example/pkg:1.0");
            let parsed =
                super::PackageVerify::try_parse_from(&argv).unwrap_or_else(|error| panic!("parse {flags:?}: {error}"));
            assert_eq!(
                parsed.content_mode(),
                expected,
                "flags {flags:?} must select {expected:?}",
            );
        }
    }

    /// `--type` narrows a search that only attestation mode performs, so clap
    /// refuses it alone (`requires = "attestation"`). Asserted because the
    /// alternative — accepting it and ignoring it — is silent.
    #[test]
    fn type_without_attestation_is_a_usage_error() {
        // `let ... else` rather than `expect_err`: the Ok type is the clap
        // struct, which carries no `Debug` (no sibling command's does either).
        let Err(error) = super::PackageVerify::try_parse_from([
            "verify",
            "-p",
            "linux/amd64",
            "--type",
            "cyclonedx",
            "registry.example/pkg:1.0",
        ]) else {
            panic!("--type alone must not parse");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    /// The `signatures[]` row is projected from the pipeline's own result, not
    /// rebuilt from the flat report fields.
    ///
    /// The acceptance-level version of this — a real `ocx package verify
    /// --format json` against a signed artifact — is not writable in this tree:
    /// the sign path still emits the pre-parity `messageSignature` bundle shape
    /// that D-2's reader refuses, so every positive-path verify in
    /// `test/tests/test_verify.py` is red for a reason that has nothing to do
    /// with this projection. Asserted here instead, over the same
    /// `VerifyResult` the pipeline returns.
    #[test]
    fn the_signature_row_carries_what_the_pipeline_verified() {
        use ocx_oci::Digest;
        use ocx_sign::sign::SignatureFormat;
        use ocx_sign::verify::{DiscoveryMethod, VerifyResult};
        use ocx_trust::key_ref::KeyBackendKind;

        let verified = VerifyResult {
            subject_digest: Digest::Sha256("a".repeat(64)),
            referrer_digest: Digest::Sha256("b".repeat(64)),
            key_backend: KeyBackendKind::Keyless,
            certificate_identity: Some("ocx-test@example.com".into()),
            certificate_oidc_issuer: Some("http://dex:5556/dex".into()),
            signed_at: Some(1_787_969_275),
            signature_format: SignatureFormat::Simplesigning,
            discovery_method: DiscoveryMethod::SidecarTag,
            rekor_log_index: Some(11),
        };

        let row = serde_json::to_value(PackageVerify::signature_entry(&verified)).expect("the row serializes");
        // The three fields only a multi-signature listing states, and the three
        // the flat report also carries — all read off the verified candidate, so
        // a row can never describe a different signature from the one that
        // passed.
        assert_eq!(row["signature_format"], "simplesigning");
        assert_eq!(row["discovery_method"], "sidecar_tag");
        assert_eq!(row["key_backend"], "keyless");
        assert_eq!(row["referrer_digest"], format!("sha256:{}", "b".repeat(64)));
        assert_eq!(row["certificate_identity"], "ocx-test@example.com");
        assert_eq!(row["certificate_oidc_issuer"], "http://dex:5556/dex");
        assert_eq!(row["rekor_log_index"], 11);
        assert_eq!(
            row["signed_at"],
            package_sign_common::iso8601(1_787_969_275),
            "the row states the instant in the same spelling the flat report does",
        );

        // A key-mode candidate with no Rekor upload: the absences survive the
        // projection as absences, never as empty strings — the whole reason the
        // typed `Option`s exist beside the flat `String` fields.
        let key_mode = VerifyResult {
            key_backend: KeyBackendKind::File,
            certificate_identity: None,
            certificate_oidc_issuer: None,
            signed_at: None,
            rekor_log_index: None,
            ..verified
        };
        let row = serde_json::to_value(PackageVerify::signature_entry(&key_mode)).expect("the row serializes");
        let object = row.as_object().expect("a row is an object");
        for absent in [
            "certificate_identity",
            "certificate_oidc_issuer",
            "signed_at",
            "rekor_log_index",
        ] {
            assert!(!object.contains_key(absent), "{absent} must be absent, not empty");
        }
    }

    use super::*;
}
