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
use ocx_lib::oci::sign::{SignError, SignErrorKind};
use ocx_lib::package_manager::{AttestOptions, SweptOutcome};

use crate::api::data::attestation::AttestationReport;
use crate::api::data::sweep::{SweepReport, SweptTagReport};
use crate::command::package_sign_common;
use crate::options;
use crate::options::key::KeyOpt;
use crate::options::rekor_upload::RekorUploadOpt;
use crate::options::signature_format::SignatureFormatOpt;
use crate::options::tags::TagsOpt;

#[derive(Parser, Clone)]
pub struct PackageAttest {
    /// Narrow into one platform of an image index.
    ///
    /// Omit it to act on whatever the reference resolves to: an index is then
    /// the subject itself, which is where cosign puts a multi-platform tag's
    /// attestation. Given against a reference that resolves to a single manifest,
    /// there is nothing to narrow and the command fails.
    ///
    /// Refused alongside `--tags` / `--tags-file`: a sweep is about indices by
    /// definition, and narrowing into one index to reach a child `push` already
    /// signed contradicts it.
    #[clap(
        short = 'p',
        long = "platform",
        value_name = "PLATFORM",
        conflicts_with_all = ["tags", "tags_file"]
    )]
    platform: Option<oci::Platform>,

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
    ///
    /// Keyless-only: an error alongside `--key`, never silently ignored.
    #[clap(long = "fulcio-url", value_name = "URL", conflicts_with = "key")]
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
        conflicts_with = "identity_token_stdin",
        conflicts_with = "key"
    )]
    identity_token_file: Option<std::path::PathBuf>,

    /// Read the OIDC identity token from stdin (second precedence).
    ///
    /// Mutually exclusive with `--identity-token-file`. Accepts a newline-terminated
    /// token on stdin; trailing whitespace is trimmed.
    #[clap(
        long = "identity-token-stdin",
        conflicts_with = "identity_token_file",
        conflicts_with = "key"
    )]
    identity_token_stdin: bool,

    /// Suppress the interactive browser OAuth fallback (CI / headless).
    #[clap(long = "no-tty", conflicts_with = "key")]
    no_tty: bool,

    /// Bypass the referrers-capability cache for this invocation.
    #[clap(long = "no-cache")]
    no_cache: bool,

    #[clap(flatten)]
    signature_format: SignatureFormatOpt,

    #[clap(flatten)]
    key: KeyOpt,

    #[clap(flatten)]
    rekor_upload: RekorUploadOpt,

    #[clap(flatten)]
    tags: TagsOpt,

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

        // Parsed once, here: `KeyRefError` decides between an unimplemented
        // backend (exit 85) and a malformed reference (exit 64). Wrapped in
        // `SignError` before it reaches `anyhow` for the reason `sign` states
        // at the same call: `classify_error` downcasts the outer error, so a
        // bare kind exits 1 and carries no identifier.
        let key = self
            .key
            .reference()
            .map_err(|kind| SignError::new(identifier.clone(), SignErrorKind::from(kind)))?;
        let configured_rekor_upload = context
            .config_trust_sigstore()
            .and_then(|sigstore| sigstore.rekor_upload);
        let rekor_upload = self
            .rekor_upload
            .enabled(self.key.is_key_mode(), configured_rekor_upload)
            .map_err(|kind| SignError::new(identifier.clone(), kind))?;

        let options = AttestOptions {
            fulcio_url,
            rekor_url,
            identity_token,
            key,
            rekor_upload,
            format: self.signature_format.write_format(),
            predicate_type: self.predicate_type.clone(),
            predicate,
            no_cache: self.no_cache,
            no_tty: self.no_tty,
            offline: context.is_offline(),
        };
        // The sweep branches here, once the option set is complete, so every
        // step above is byte-identical on both paths.
        // `is_sweep`, not "the resolved list is non-empty": an empty
        // `--tags-file` is still a sweep of zero tags, and falling through to
        // the single-reference path there would sign the reference the caller
        // named instead of the nothing the file asked for.
        if self.tags.is_sweep() {
            let tags = self.tags.resolve().await?;
            return self.sweep(&context, &identifier, &tags, &options).await;
        }

        let result = context
            .manager()
            .attest_one(&identifier, self.platform.as_ref(), options)
            .await
            .map_err(package_sign_common::attest_error_into_anyhow)?
            .result;

        let report = AttestationReport::new(identifier.to_string(), self.platform.as_ref(), result);
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }

    /// Attest the index each swept tag resolves to, one row per tag.
    ///
    /// The reporting half of [`PackageManager::attest_tags`]; the loop itself
    /// runs to completion there, so nothing here can abort early.
    ///
    /// [`PackageManager::attest_tags`]: ocx_lib::package_manager::PackageManager::attest_tags
    async fn sweep(
        &self,
        context: &crate::app::Context,
        identifier: &oci::Identifier,
        tags: &[String],
        options: &AttestOptions,
    ) -> anyhow::Result<ExitCode> {
        let swept = context.manager().attest_tags(identifier, tags, options).await;

        let mut rows = Vec::with_capacity(swept.len());
        let mut failures = Vec::new();
        for entry in swept {
            let row = match entry.outcome {
                SweptOutcome::SkippedBareManifest => SweptTagReport::skipped(entry.tag),
                SweptOutcome::Failed(error) => {
                    let error = package_sign_common::attest_error_into_anyhow(*error);
                    failures.push(ocx_lib::cli::classify_error(error.as_ref()));
                    SweptTagReport::failed(
                        entry.tag,
                        None,
                        package_sign_common::error_slug("package attest", &error),
                        format!("{error:#}"),
                    )
                }
                SweptOutcome::Done(report) => SweptTagReport::completed(
                    entry.tag.clone(),
                    // `None` for the platform: a sweep acts on the index
                    // itself, which is why `--platform` is refused alongside
                    // `--tags`.
                    AttestationReport::new(identifier.clone_with_tag(entry.tag).to_string(), None, report.result),
                ),
            };
            rows.push(row);
        }

        let exit_code = package_sign_common::sweep_exit_code(&failures);
        context
            .api()
            .report(&SweepReport::new("package attest", rows, exit_code))?;
        Ok(ExitCode::from(exit_code))
    }
}

#[cfg(test)]
mod key_mode_tests {
    //! The keyless-only flags are an **error** alongside `--key`, never
    //! silently ignored — "a flag that does nothing" is the failure mode this
    //! spec rejects everywhere else. The rejection is clap's, declared on the
    //! frozen `key` arg id, so these parse rather than execute.

    use super::*;

    /// The flags that mean nothing under a key: Fulcio issues no certificate,
    /// no OIDC identity is redeemed, and there is no browser flow to suppress.
    const KEYLESS_ONLY_FLAGS: &[&[&str]] = &[
        &["--fulcio-url", "https://fulcio.example"],
        &["--identity-token-file", "token.txt"],
        &["--identity-token-stdin"],
        &["--no-tty"],
    ];

    /// Parse `argv` as this command, returning clap's error kind on refusal.
    fn parse(argv: &[&str]) -> Result<(), clap::error::ErrorKind> {
        PackageAttest::try_parse_from(argv).map(|_| ()).map_err(|e| e.kind())
    }

    #[test]
    fn every_keyless_only_flag_is_refused_alongside_a_key() {
        for flag in KEYLESS_ONLY_FLAGS {
            let mut argv = vec!["attest"];
            argv.extend_from_slice(&[
                "--platform",
                "linux/amd64",
                "--predicate",
                "p.json",
                "--type",
                "cyclonedx",
            ]);
            argv.extend_from_slice(&["--key", "cosign.key"]);
            argv.extend_from_slice(flag);
            argv.push("registry.example/pkg:1.0");
            assert_eq!(
                parse(&argv),
                Err(clap::error::ErrorKind::ArgumentConflict),
                "`{flag:?}` must be refused alongside --key, not ignored",
            );
        }
    }

    /// The other half: each of those flags is perfectly legal on its own, so
    /// the test above is measuring the conflict rather than a broken command.
    #[test]
    fn every_keyless_only_flag_is_accepted_without_a_key() {
        for flag in KEYLESS_ONLY_FLAGS {
            let mut argv = vec!["attest"];
            argv.extend_from_slice(&[
                "--platform",
                "linux/amd64",
                "--predicate",
                "p.json",
                "--type",
                "cyclonedx",
            ]);
            argv.extend_from_slice(flag);
            argv.push("registry.example/pkg:1.0");
            assert_eq!(parse(&argv), Ok(()), "`{flag:?}` must be legal keyless");
        }
    }

    /// `--rekor-upload` and `--no-rekor-upload` both parse; which one is legal
    /// depends on the key model, and that decision is `RekorUploadOpt`'s, not
    /// clap's. Declaring `requires = "key"` would print "the following required
    /// arguments were not provided: --key", inverting the reason.
    #[test]
    fn no_rekor_upload_parses_under_keyless_so_the_refusal_can_name_its_reason() {
        let mut argv = vec!["attest"];
        argv.extend_from_slice(&[
            "--platform",
            "linux/amd64",
            "--predicate",
            "p.json",
            "--type",
            "cyclonedx",
        ]);
        argv.extend_from_slice(&["--no-rekor-upload"]);
        argv.push("registry.example/pkg:1.0");
        assert_eq!(
            parse(&argv),
            Ok(()),
            "clap must accept it so `RekorUploadOpt::enabled` can refuse it with the reason",
        );
    }
}

#[cfg(test)]
mod platform_optionality_tests {
    //! WP1: `--platform` stopped being `required`.
    //!
    //! Parse-level, because the change IS clap's: `required = true` turned
    //! `ocx package attest <ref>` into a usage error (64) before the pipeline ever
    //! ran, so no behavioural test further down could observe the widening.

    use super::*;

    /// The reference every case names, so each varies exactly one thing.
    const REFERENCE: &str = "registry.example/pkg:1.0";

    /// Every argument this command needs *besides* `--platform` and the
    /// reference.
    const REQUIRED: &[&str] = &["attest", "--predicate", "p.json", "--type", "cyclonedx"];

    /// The parsed `--platform`, or clap's error kind on refusal. `extra` is
    /// appended after the required arguments.
    fn parse(extra: &[&str]) -> Result<Option<oci::Platform>, clap::error::ErrorKind> {
        let mut argv = REQUIRED.to_vec();
        argv.extend_from_slice(extra);
        PackageAttest::try_parse_from(argv)
            .map(|parsed| parsed.platform)
            .map_err(|error| error.kind())
    }

    /// The widening itself: a bare reference with no `-p` parses, and the
    /// absence reaches the command as `None` rather than as a default.
    #[test]
    fn a_reference_with_no_platform_parses_to_none() {
        assert_eq!(parse(&[REFERENCE]), Ok(None));
    }

    /// The other half — the flag still parses when given, in both spellings —
    /// so the test above measures optionality rather than a deleted flag.
    #[test]
    fn the_flag_still_parses_in_both_spellings() {
        let expected = Ok(Some("linux/amd64".parse::<oci::Platform>().expect("platform")));
        assert_eq!(parse(&["--platform", "linux/amd64", REFERENCE]), expected);
        assert_eq!(parse(&["-p", "linux/amd64", REFERENCE]), expected);
    }

    /// Optional means the flag may be absent, never that it may be empty: a
    /// valueless `--platform` is still a usage error.
    #[test]
    fn the_flag_still_requires_a_value_when_given() {
        assert!(
            parse(&[REFERENCE, "--platform"]).is_err(),
            "a valueless --platform must not parse",
        );
    }
}

#[cfg(test)]
mod signature_format_tests {
    //! WP4: `--signature-format` reaches `attest` with the same grammar `sign`
    //! has, and an unknown value is clap's error rather than a silent fallback
    //! to the default.

    use super::*;
    use ocx_lib::oci::sign::SignatureFormat;

    const REFERENCE: &str = "registry.example/pkg:1.0";

    fn parse(args: &[&str]) -> Result<SignatureFormat, clap::error::ErrorKind> {
        let mut argv = vec!["attest", "--predicate", "p.json", "--type", "cyclonedx"];
        argv.extend_from_slice(args);
        argv.push(REFERENCE);
        PackageAttest::try_parse_from(argv)
            .map(|parsed| parsed.signature_format.write_format())
            .map_err(|error| error.kind())
    }

    /// Unset writes a bundle, and each named value reaches the write side
    /// verbatim — the same table `SignatureFormatOpt`'s own test walks, driven
    /// here through *this* command so a missing `#[clap(flatten)]` reds.
    #[test]
    fn every_value_reaches_the_write_side_and_the_default_is_bundle() {
        assert_eq!(parse(&[]), Ok(SignatureFormat::Bundle));
        for (value, expected) in [
            ("bundle", SignatureFormat::Bundle),
            ("simplesigning", SignatureFormat::Simplesigning),
            ("both", SignatureFormat::Both),
        ] {
            assert_eq!(parse(&["--signature-format", value]), Ok(expected), "value `{value}`");
        }
    }

    /// The negative half: an unknown value is refused rather than defaulted,
    /// so the test above measures the flag rather than a `None` fallback.
    #[test]
    fn an_unknown_format_is_refused() {
        assert_eq!(
            parse(&["--signature-format", "dsse"]),
            Err(clap::error::ErrorKind::InvalidValue),
        );
        assert_eq!(
            parse(&["--signature-format"]),
            Err(clap::error::ErrorKind::InvalidValue),
            "a valueless --signature-format must not parse",
        );
    }
}

#[cfg(test)]
mod sweep_exclusivity_tests {
    //! `--platform` is refused alongside `--tags` and `--tags-file`, for the
    //! reason it is on `sign`: a sweep is about indices, and narrowing into one
    //! contradicts it. The refusal is clap's, so these parse rather than
    //! execute.

    use super::*;

    /// The reference every case names, so each varies exactly one thing.
    const REFERENCE: &str = "registry.example/pkg:1.0";

    /// Every argument this command needs besides the ones under test.
    const REQUIRED: &[&str] = &["attest", "--predicate", "p.json", "--type", "cyclonedx"];

    /// The two spellings of a sweep, each of which `--platform` must refuse.
    const SWEEP_FLAGS: &[&[&str]] = &[&["--tags", "3.28"], &["--tags-file", "tags.txt"]];

    fn parse(extra: &[&str]) -> Result<(), clap::error::ErrorKind> {
        let mut argv = REQUIRED.to_vec();
        argv.extend_from_slice(extra);
        argv.push(REFERENCE);
        PackageAttest::try_parse_from(argv).map(|_| ()).map_err(|e| e.kind())
    }

    #[test]
    fn a_platform_is_refused_alongside_either_sweep_flag() {
        for sweep in SWEEP_FLAGS {
            let mut argv = vec!["--platform", "linux/amd64"];
            argv.extend_from_slice(sweep);
            assert_eq!(
                parse(&argv),
                Err(clap::error::ErrorKind::ArgumentConflict),
                "`{sweep:?}` must be refused alongside --platform, not silently ignored",
            );
        }
    }

    /// The other half: each flag is perfectly legal on its own.
    #[test]
    fn each_flag_parses_on_its_own() {
        for sweep in SWEEP_FLAGS {
            assert_eq!(parse(sweep), Ok(()), "`{sweep:?}` must be legal without --platform");
        }
        assert_eq!(parse(&["--platform", "linux/amd64"]), Ok(()));
    }

    /// `--tags` and `--tags-file` are a union, not alternatives.
    #[test]
    fn the_two_sweep_flags_do_not_refuse_each_other() {
        assert_eq!(parse(&["--tags", "3.28", "--tags-file", "tags.txt"]), Ok(()));
    }
}
