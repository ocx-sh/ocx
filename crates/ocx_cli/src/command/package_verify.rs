// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package verify` — keyless Sigstore verification of a target manifest's
//! signature via OCI Referrers.
//!
//! Fetches the Sigstore bundle v0.3 referrer for the target, verifies the
//! Fulcio cert chain against the resolved trust root, verifies the Rekor
//! SET, verifies the signature over the subject digest, and checks the cert
//! identity + issuer against the accepted identity. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full state machine.
//!
//! There are **no default** `--certificate-identity` / `--certificate-oidc-issuer`
//! values — keyless verification is meaningless without knowing whose
//! signature you trust. The pair may come from the flags or from a
//! `[[trust.policy]]` entry whose scope covers the target; the flags are
//! optional only when such a policy matches, and are required otherwise.
//!
//! This command resolves the identifier, validates `--rekor-url` (SSRF guard),
//! resolves the trust root in precedence order — `--sigstore-trusted-root` /
//! `OCX_SIGSTORE_TRUSTED_ROOT`, then `[trust.sigstore]` from `config.toml`,
//! then `$OCX_HOME/sigstore/trusted-root.json`, then the fresh trust-root
//! cache, then the Sigstore TUF root fetched over the network — and drives the verify
//! pipeline through the [`PackageManager`](ocx_lib::package_manager) facade
//! (`verify_one`), which runs the full state machine and returns a
//! [`VerificationReport`].
//!
//! Verify reads the artifact and its signature referrer from the registry in
//! every mode. `--offline` / `OCX_OFFLINE` scopes to the Sigstore trust services
//! (the Rekor-key fetch and TUF), not the artifact registry: offline verify
//! reuses cached or supplied trust material (which must carry a pinned Rekor
//! key) and never contacts Sigstore; with no such material it fails with an
//! actionable error rather than skipping verification. A successful online
//! verify caches its trust material for later offline runs. The positive path is
//! exercised end-to-end against a real Sigstore deployment — Fulcio, Rekor,
//! TesseraCT and dex under the `sigstore` Docker Compose profile.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::cli;
use ocx_lib::oci;
use ocx_lib::oci::attest::predicate::PredicateType;
use ocx_lib::oci::verify::{VerifyContentMode, VerifyError, VerifyErrorKind};
use ocx_lib::package_manager::VerifyOptions;

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
    platform: Option<oci::Platform>,

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

    // C-S1-3 injection seam: private-Rekor override (validated in `execute`).
    // `Option`, not a clap default, so `[trust.sigstore].rekor_url` can sit
    // between the flag and the builtin.
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
    ///
    /// A keyless `sha256-<hex>.sig` or `sha256-<hex>.att` proves nothing about
    /// *when* it was signed unless its layer carries a
    /// `dev.sigstore.cosign/bundle` annotation: the Fulcio certificate it names
    /// lived about ten minutes, so without an entry a long-expired certificate
    /// is indistinguishable from a live one. Verify refuses both shapes (exit
    /// 65). Pass this in air-gapped CI, where the entry could not be fetched or
    /// was never written, and you accept a signature nothing timestamps. Inert
    /// everywhere else: a bundle's transparency evidence stays mandatory under
    /// keyless and optional under `--key`.
    #[clap(long = "allow-unlogged-signature")]
    allow_unlogged_signature: bool,

    /// Bypass the referrers-capability cache for this invocation.
    #[clap(long = "no-cache")]
    no_cache: bool,

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
    trusted_root: Option<std::path::PathBuf>,

    /// Package identifier to verify (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl PackageVerify {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // D9's format pin, resolved and refused at the invocation boundary:
        // `--signature-format both` names two shapes, and a verification result
        // cannot say "either of these satisfied me", so it is a usage error (64)
        // rather than a silent pick — before any network request rather than
        // after one. The resolved pin then decides *discovery*: the shape it
        // does not name is never looked for.
        let signature_format = self.signature_format.pin().map_err(cli::UsageError::from)?;

        // Parsed before the trust root is resolved and before any request, so
        // `--key awskms://alias/release` names its unimplemented backend (exit
        // 85) instead of being read as a filename and reported as a missing
        // file. `KeyRefError` carries which of the two it is; the `From` impl
        // is the only thing that decides 85 from 64.
        let key = self
            .key
            .reference()
            .map_err(|error| VerifyError::new(identifier.clone(), VerifyErrorKind::from(error)))?;

        // SSRF hardening (CWE-918): validate the user-supplied endpoint at the
        // boundary before it becomes an HTTP client target. Precedence, guard
        // and refusal kind are the shared ladder's — see `resolve_rekor_endpoint`.
        let rekor_url = package_sign_common::resolve_rekor_endpoint(
            context.config_trust_sigstore(),
            &identifier,
            self.rekor_url.as_deref(),
        )?;

        // Verify reads the artifact + its signature referrer from the registry in
        // every mode. `--offline` scopes to the Sigstore trust services (the
        // Rekor-key fetch and TUF), not the registry — so, unlike sign, offline
        // verify does not exit 81; it requires cached/supplied trust material
        // instead. See `verify_client`. The index the pipeline uses comes from
        // the manager facade, so only the registry client + offline flag are
        // taken here.
        let client = context.verify_client();
        let offline = context.is_offline();

        // The trust-root cache is keyed by the Rekor instance; compute the key
        // here (where `rekor_url`'s type is in scope) so the resolver takes a
        // plain string and the CLI need not name `url::Url`.
        let rekor_cache_key = ocx_lib::oci::verify::trust_cache::cache_key_for_rekor(&rekor_url);
        let trust_root = package_sign_common::resolve_trust_root(
            &context,
            &identifier,
            &rekor_cache_key,
            offline,
            self.trusted_root.as_deref(),
        )
        .await?;

        // Resolve the identity constraints: flag override (exact pair), or the
        // scope-matched [[trust.policy]] set pooled across config.toml tiers +
        // the project ocx.toml.
        let policies = package_sign_common::resolve_policies(
            &context,
            &identifier,
            self.certificate_identity.as_deref(),
            self.certificate_oidc_issuer.as_deref(),
            key.as_ref(),
        )
        .await?;

        // Route through the PackageManager facade: it assembles the verify
        // pipeline (registry client, index) and returns a per-package error
        // whose kind preserves the verify exit-code taxonomy.
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
            // `signatures[]` reports every signature the subject carries, so
            // the scan has to look at every candidate. The install-time
            // auto-verify hook renders no report and deliberately does not.
            report_all: true,
        };
        let verified = context
            .manager()
            .verify_one(&identifier, self.platform.as_ref(), options)
            .await
            .map_err(package_sign_common::verify_error_into_anyhow)?
            .signatures;

        // Built before the verdict is moved out: every verified candidate gets a
        // row, the verdict included, so `signatures[0]` and the flat fields
        // describe the same signature.
        let signatures: Vec<SignatureEntry> = verified.iter().map(Self::signature_entry).collect();
        // Non-empty by `VerifyPipeline::run`'s contract; the verdict is the
        // first candidate that fully passed, under either arity.
        let Some(result) = verified.into_iter().next() else {
            unreachable!("a successful verify returns at least one signature");
        };

        // The flat report predates the key model and its three certificate
        // fields are `String`. Under a key they are genuinely absent, and the
        // empty string is how this shape spells that — the typed absence
        // survives on `VerifyResult` and reaches JSON through `signatures[]`.
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

    /// Project one verified candidate onto the frozen `signatures[]` row.
    ///
    /// A borrow, not a move: the verdict is the first of the same list and the
    /// flat fields are read off it afterwards. Nothing here is rendered in
    /// plain text — see the `SignatureEntry` struct note (CWE-150).
    fn signature_entry(result: &ocx_lib::oci::verify::VerifyResult) -> SignatureEntry {
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

    /// The kind of signed content to verify: a bare artifact signature, or an
    /// in-toto attestation optionally narrowed to one predicate type.
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
        use ocx_lib::oci::attest::predicate::PredicateType;

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
        use ocx_lib::oci::Digest;
        use ocx_lib::oci::sign::{KeyBackendKind, SignatureFormat};
        use ocx_lib::oci::verify::{DiscoveryMethod, VerifyResult};

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
