// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package attest` — attach an in-toto attestation to a published
//! package manifest as a DSSE-enveloped Sigstore bundle, via OCI Referrers.
//!
//! The keyless machinery is `ocx package sign`'s, unchanged: same Fulcio/Rekor
//! endpoints, same OIDC token precedence, same referrers publish. What differs
//! is the payload — a signed in-toto Statement wrapping a caller-supplied
//! predicate document, rather than a signature over the manifest digest.
//!
//! Token handling is reused verbatim from `package_sign_common`: there is
//! deliberately NO `--identity-token <VALUE>` flag, because a raw token on the
//! command line leaks into shell history and `ps`.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;
use ocx_lib::oci::attest::predicate::PredicateType;
use ocx_lib::oci::sign::SignErrorKind;
use ocx_lib::package_manager::AttestOptions;

use crate::api::data::attestation::AttestationReport;
use crate::command::package_sign_common;
use crate::options;

#[derive(Parser, Clone)]
pub struct PackageAttest {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    /// Predicate document to attach (JSON).
    ///
    /// Read before any network call, so a bad path costs no round-trip.
    ///
    /// Security: on Unix the file is opened without following symlinks, and a
    /// symlink at this path is refused. Whatever a link points at would
    /// otherwise be embedded, signed with your identity and published to an
    /// append-only log, and that is not undoable.
    #[clap(long = "predicate", required = true, value_name = "PATH")]
    predicate: std::path::PathBuf,

    /// Predicate type: an alias or a full URI.
    ///
    /// Aliases: cyclonedx, spdx, spdxjson, slsaprovenance, slsaprovenance02,
    /// slsaprovenance1, link, vuln, openvex, custom. Anything else must be an
    /// absolute URI, stored exactly as spelled.
    #[clap(long = "type", required = true, value_name = "TYPE")]
    predicate_type: PredicateType,

    // `Option`, not a clap default: `execute` has to tell an explicit flag
    // from an absent one, because `[trust.sigstore]` sits between them.
    /// Fulcio CA endpoint (the keyless certificate issuer)
    ///
    /// Defaults to [trust.sigstore].fulcio_url, else public Fulcio.
    #[clap(long = "fulcio-url", value_name = "URL")]
    fulcio_url: Option<String>,

    /// Rekor transparency-log endpoint
    ///
    /// Defaults to [trust.sigstore].rekor_url, else public Rekor.
    #[clap(long = "rekor-url", value_name = "URL")]
    rekor_url: Option<String>,

    /// Read the OIDC identity token from this file (highest precedence).
    ///
    /// Use this when the CI system writes the token to a file instead of the
    /// environment.
    ///
    /// Security: `--identity-token-file` does not follow symlinks. On Unix the
    /// file is opened with `O_NOFOLLOW` and rejected if not owned by the
    /// effective user or if group/other permission bits are set.
    #[clap(
        long = "identity-token-file",
        value_name = "PATH",
        conflicts_with = "identity_token_stdin"
    )]
    identity_token_file: Option<std::path::PathBuf>,

    /// Read the OIDC identity token from stdin (second precedence).
    ///
    /// Mutually exclusive with `--identity-token-file`. Accepts a newline-terminated
    /// token on stdin; trailing whitespace is trimmed.
    #[clap(long = "identity-token-stdin", conflicts_with = "identity_token_file")]
    identity_token_stdin: bool,

    /// Suppress the interactive browser OAuth fallback (CI / headless).
    #[clap(long = "no-tty")]
    no_tty: bool,

    /// Bypass the referrers-capability cache for this invocation.
    #[clap(long = "no-cache")]
    no_cache: bool,

    /// Package identifier to attest (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl PackageAttest {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // SSRF hardening (CWE-918) at the boundary, before either URL becomes
        // an HTTP target. Precedence, guard and refusal kind are the shared
        // ladder's — see `resolve_sigstore_pair`.
        let (fulcio_url, rekor_url) = package_sign_common::resolve_sigstore_pair(
            context.config_trust_sigstore(),
            &identifier,
            self.fulcio_url.as_deref(),
            self.rekor_url.as_deref(),
        )?;

        // Attesting is signing, so an offline attest is a deliberate refusal
        // (77), not a transport failure. Runs before the predicate read and
        // before token resolution, so a refused run touches no credential.
        //
        // WATCH: offline-before-token is S-002's contract, pinned end to end in
        // WP10a. Moving this below `resolve_override_token` still exits 77 and
        // still passes every unit test, while reading a credential for a run
        // that was already refused.
        package_sign_common::refuse_when_offline(&context, &identifier, SignErrorKind::OfflineAttestRefused)?;

        let predicate = package_sign_common::read_predicate(&self.predicate, &identifier).await?;

        // Token precedence: file > stdin > OCX_IDENTITY_TOKEN. Held under
        // `Zeroizing`; never logged, never surfaced in error context.
        let identity_token = package_sign_common::resolve_override_token(
            self.identity_token_file.as_deref(),
            self.identity_token_stdin,
            &identifier,
        )
        .await?;

        let options = AttestOptions {
            fulcio_url,
            rekor_url,
            identity_token,
            predicate_type: self.predicate_type.clone(),
            predicate,
            no_cache: self.no_cache,
            no_tty: self.no_tty,
            offline: context.is_offline(),
        };
        let result = context
            .manager()
            .attest_one(&identifier, &self.platform, options)
            .await
            .map_err(package_sign_common::attest_error_into_anyhow)?
            .result;

        let report = AttestationReport::new(identifier.to_string(), &self.platform, result);
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }
}
