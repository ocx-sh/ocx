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

use ocx_lib::oci;
use ocx_lib::oci::attest::predicate::PredicateType;
use ocx_lib::oci::verify::VerifyContentMode;
use ocx_lib::package_manager::VerifyOptions;

use crate::api::data::verification::VerificationReport;
use crate::command::package_sign_common;
use crate::options;

#[derive(Parser, Clone)]
pub struct PackageVerify {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    /// Expected certificate SAN (exact match).
    ///
    /// Optional when a `[trust.policy]` whose scope covers the target supplies
    /// the identity; when given, this flag and `--certificate-oidc-issuer`
    /// override any policy. The two flags are used together; supplying one
    /// without the other is an error.
    ///
    /// Example: `you@example.com`, `https://github.com/org/repo/.github/workflows/build.yml@refs/heads/main`.
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
    ///
    /// Example: `https://github.com/login/oauth`, `https://token.actions.githubusercontent.com`.
    #[clap(
        long = "certificate-oidc-issuer",
        value_name = "URL",
        requires = "certificate_identity"
    )]
    certificate_oidc_issuer: Option<String>,

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
        };
        let result = context
            .manager()
            .verify_one(&identifier, &self.platform, options)
            .await
            .map_err(package_sign_common::verify_error_into_anyhow)?
            .result;

        let report = VerificationReport::new(
            result.subject_digest,
            result.referrer_digest,
            result.certificate_identity,
            result.certificate_oidc_issuer,
            package_sign_common::iso8601(result.signed_at),
        );
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
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

    use super::*;
}
