// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package sign` — keyless Sigstore signing of a published package
//! manifest via OCI Referrers.
//!
//! Publishes a Sigstore bundle v0.3 as a referrer manifest for the target,
//! with the bundle body itself in a CAS blob. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full pipeline.
//!
//! C-S1-4 override token handling: the CLI resolves `--identity-token-file` >
//! `--identity-token-stdin` > `OCX_IDENTITY_TOKEN` env *before* calling the
//! sign pipeline. There is deliberately NO `--identity-token <VALUE>` flag —
//! raw tokens on the command line would leak into shell history.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::Error as LibError;
use ocx_lib::oci;
use ocx_lib::oci::sign::{SignError, SignErrorKind};
use ocx_lib::package_manager::error::{PackageError, PackageErrorKind};
use ocx_lib::package_manager::{SignOptions, SweptOutcome};

use crate::api::data::sweep::{SweepReport, SweptTagReport};
use crate::command::package_sign_common;
use crate::options;
use crate::options::key::KeyOpt;
use crate::options::rekor_upload::RekorUploadOpt;
use crate::options::signature_format::SignatureFormatOpt;
use crate::options::tags::TagsOpt;

#[derive(Parser, Clone)]
pub struct PackageSign {
    /// Narrow into one platform of an image index.
    ///
    /// Omit it to act on whatever the reference resolves to: an index is then
    /// the subject itself, which is where cosign puts a multi-platform tag's
    /// signature. Given against a reference that resolves to a single manifest,
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

    // C-S1-3 injection seam: these two URL overrides point sign at a private
    // Fulcio/Rekor deployment (validated at the boundary in `execute`). Left
    // `Option` rather than clap-defaulted so `execute` can tell "user passed
    // the public default" from "user passed nothing" — the latter is what
    // `[trust.sigstore]` gets to answer.
    /// Fulcio CA endpoint (the keyless certificate issuer)
    ///
    /// Defaults to [trust.sigstore].fulcio_url, else public Fulcio.
    ///
    /// Keyless-only: an error alongside `--key`, never silently ignored. A flag
    /// that does nothing is the failure mode this command refuses everywhere.
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
    /// environment (GitHub Actions `$ACTIONS_ID_TOKEN_REQUEST_TOKEN` flow is
    /// env-based; other systems write the token out).
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
    ///
    /// When set, ambient detection must succeed or the override flags must
    /// supply a token; there is no interactive recovery path.
    #[clap(long = "no-tty", conflicts_with = "key")]
    no_tty: bool,

    /// Bypass the referrers-capability cache for this invocation.
    ///
    /// Default: the per-registry capability probe is cached in
    /// `$OCX_HOME/state/referrers/<registry>.json` to avoid repeated 404
    /// probes. `--no-cache` forces a fresh probe, useful after a registry
    /// upgrades to OCI 1.1.
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

    /// Package identifier to sign (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl PackageSign {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // SSRF hardening (CWE-918): validate user-supplied endpoint URLs at the
        // boundary before they become HTTP client targets. Precedence, guard
        // and refusal kind are the shared ladder's — see `resolve_sigstore_pair`.
        let (fulcio_url, rekor_url) = package_sign_common::resolve_sigstore_pair(
            context.config_trust_sigstore(),
            &identifier,
            self.fulcio_url.as_deref(),
            self.rekor_url.as_deref(),
        )?;
        // S1-E policy: offline sign is a deliberate rejection, NOT a passive
        // network-access failure — the acceptance test `test_sign_offline_refused`
        // drives this contract.
        package_sign_common::refuse_when_offline(&context, &identifier, SignErrorKind::OfflineSignRefused)?;

        // C-S1-4 token precedence: file > stdin > env. The resolved token is
        // held under `Zeroizing`; never log, never surface in error context.
        let override_token = package_sign_common::resolve_override_token(
            self.identity_token_file.as_deref(),
            self.identity_token_stdin,
            &identifier,
        )
        .await?;

        // Route through the PackageManager facade: it owns the pipeline assembly
        // (registry client, index, signer, token provider) and returns a
        // per-package error whose kind preserves the sign exit-code taxonomy.
        // Offline was already refused above via `OfflineSignRefused`.
        // The reference is parsed once, here: `KeyRefError` decides between an
        // unimplemented backend (exit 85) and a malformed reference (exit 64),
        // and `SignErrorKind::from` is the single place that routing lives.
        //
        // Wrapped in `SignError` before it reaches `anyhow`, exactly as verify
        // wraps its twin: `classify_error` downcasts the outer `SignError`, so
        // a bare kind on the chain matches nothing and falls through to
        // `Failure` (1) — the 85/64 rows would be unreachable, and
        // `envelope.error.context.identifier` would be empty.
        let key = self
            .key
            .reference()
            .map_err(|kind| SignError::new(identifier.clone(), SignErrorKind::from(kind)))?;
        // Keyless always uploads and `--no-rekor-upload` is an error there; key
        // mode is off unless opted in, per-run or fleet-wide. A Fulcio
        // certificate is valid for about ten minutes, so under keyless the
        // Rekor timestamp is the only durable proof the signature happened
        // while the certificate still was.
        let configured_rekor_upload = context
            .config_trust_sigstore()
            .and_then(|sigstore| sigstore.rekor_upload);
        let rekor_upload = self
            .rekor_upload
            .enabled(self.key.is_key_mode(), configured_rekor_upload)
            .map_err(|kind| SignError::new(identifier.clone(), kind))?;

        let options = SignOptions {
            fulcio_url,
            rekor_url,
            identity_token: override_token,
            no_cache: self.no_cache,
            no_tty: self.no_tty,
            key,
            rekor_upload,
            format: self.signature_format.write_format(),
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
            .sign_one(&identifier, self.platform.as_ref(), options)
            .await
            .map_err(sign_error_into_anyhow)?
            .result;

        // Read before the result is consumed: a `--signature-format both` run
        // where one leg failed still reports the leg that landed, and the exit
        // code comes from the failure rather than from the run as a whole.
        let failure = result.first_failure().map(package_sign_common::leg_exit_code);
        let report = package_sign_common::signature_report(&identifier, self.platform.as_ref(), result)
            // The report is the one stdout document of a partially-failed run,
            // so its envelope has to carry the code the process exits with. A
            // success envelope hard-codes 0, and `error_envelope.rs` states the
            // invariant this would otherwise break: the envelope's `exit_code`
            // can never disagree with the process's.
            .with_exit_code(failure.unwrap_or(ocx_lib::cli::ExitCode::Success));
        context.api().report(&report)?;
        Ok(failure.map_or(ExitCode::SUCCESS, ExitCode::from))
    }

    /// Sign the index each swept tag resolves to, one row per tag.
    ///
    /// The loop itself is [`PackageManager::sign_tags`]; this is the reporting
    /// half — turn each outcome into a row, collect the failures' exit codes,
    /// and let [`package_sign_common::sweep_exit_code`] pick the one the
    /// process returns. Nothing here can abort early: the sweep already ran to
    /// completion, which is the contract.
    ///
    /// [`PackageManager::sign_tags`]: ocx_lib::package_manager::PackageManager::sign_tags
    async fn sweep(
        &self,
        context: &crate::app::Context,
        identifier: &oci::Identifier,
        tags: &[String],
        options: &SignOptions,
    ) -> anyhow::Result<ExitCode> {
        let swept = context.manager().sign_tags(identifier, tags, options).await;

        let mut rows = Vec::with_capacity(swept.len());
        let mut failures = Vec::new();
        for entry in swept {
            let row = match entry.outcome {
                SweptOutcome::SkippedBareManifest => SweptTagReport::skipped(entry.tag),
                SweptOutcome::Failed(error) => {
                    let error = sign_error_into_anyhow(*error);
                    failures.push(ocx_lib::cli::classify_error(error.as_ref()));
                    SweptTagReport::failed(
                        entry.tag,
                        None,
                        package_sign_common::error_slug("package sign", &error),
                        format!("{error:#}"),
                    )
                }
                SweptOutcome::Done(report) => {
                    let result = report.result;
                    // Read before the result is consumed, exactly as the
                    // single-reference path does: a swept tag whose `both` run
                    // lost one leg is a failure that still carries the leg that
                    // landed.
                    let leg = result
                        .first_failure()
                        .map(|kind| (package_sign_common::leg_exit_code(kind), kind.to_string()));
                    let signature = swept_signature_report(identifier, &entry.tag, result);
                    match leg {
                        Some((code, message)) => {
                            failures.push(code);
                            SweptTagReport::failed(
                                entry.tag,
                                Some(signature),
                                package_sign_common::category_slug(code),
                                message,
                            )
                        }
                        None => SweptTagReport::completed(entry.tag, signature),
                    }
                }
            };
            rows.push(row);
        }

        let exit_code = package_sign_common::sweep_exit_code(&failures);
        context
            .api()
            .report(&SweepReport::new("package sign", rows, exit_code))?;
        Ok(ExitCode::from(exit_code))
    }
}

/// The signature report one swept tag's row carries.
///
/// The report's `identifier` is the tag **this** iteration signed, not the
/// positional the sweep was launched from: a row reading `tag: "1.0.0"` beside
/// `identifier: "repo:9.9.9"` names an artifact the run never touched.
/// `attest`'s sweep does the same thing one file over.
///
/// Split out of [`PackageSign::sweep`] only so the choice is reachable by a
/// test — the sweep itself needs a live `PackageManager`, so nothing that can
/// run in-process could otherwise read the row back.
fn swept_signature_report(
    identifier: &oci::Identifier,
    tag: &str,
    result: ocx_lib::oci::sign::SignResult,
) -> crate::api::data::signature::SignatureReport {
    package_sign_common::signature_report(&identifier.clone_with_tag(tag), None, result)
}

/// Convert a sign-path [`PackageError`] into an `anyhow::Error`, unwrapping the
/// inner [`SignError`] so the `--format json` error envelope's
/// `context.identifier` is populated on every pipeline-stage failure — matching
/// the pre-check paths (offline refusal, URL validation) that already surface a
/// bare `SignError`.
///
/// `ocx_lib::Error::Sign` is `#[error(transparent)]`, so its `source()` forwards
/// straight to the inner `SignErrorKind`, skipping the `SignError` node the
/// envelope's context walk downcasts to. The exit code, `error.kind`, and
/// `error.detail` are unchanged — all three reach the same `SignErrorKind`
/// whether or not the `SignError` node is preserved.
fn sign_error_into_anyhow(err: PackageError) -> anyhow::Error {
    match err.kind {
        PackageErrorKind::Internal(LibError::Sign(sign_error)) => anyhow::Error::new(*sign_error),
        kind => anyhow::Error::new(kind),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the sign-path error-envelope contract.

    use super::*;
    use ocx_lib::oci::sign::SignError;

    fn test_identifier() -> oci::Identifier {
        oci::Identifier::parse("registry.example/pkg:1.0").expect("static parse")
    }

    /// A pipeline-stage `SignError` wrapped in a `PackageError` (the shape the
    /// sign facade produces) must still surface `context.identifier` in the
    /// `--format json` envelope.
    ///
    /// Regression guard for the `sign_error_into_anyhow` unwrap — mirror of the
    /// verify-side test. `PackageError` omits `#[source]` and `Error::Sign` is
    /// `#[error(transparent)]`, so only the unwrap re-roots the chain on the bare
    /// `SignError` the envelope's context walk downcasts to. If the unwrap
    /// regresses the identifier vanishes and this fails.
    #[test]
    fn sign_error_wrapped_in_package_error_still_populates_envelope_identifier() {
        use crate::error_envelope::render_error_envelope;

        let id = test_identifier();
        let package_error = PackageError::new(
            id.clone(),
            PackageErrorKind::Internal(LibError::Sign(Box::new(SignError::new(
                id,
                SignErrorKind::OidcTokenRejected,
            )))),
        );
        let err = sign_error_into_anyhow(package_error);
        let json = render_error_envelope("package sign", &err).expect("render envelope");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(parsed["exit_code"], 80);
        assert_eq!(parsed["error"]["kind"], "auth_error");
        assert_eq!(
            parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0",
            "identifier must survive the PackageError wrap → sign_error_into_anyhow unwrap",
        );
    }

    /// A sweep row names the tag it signed, never the positional the sweep was
    /// launched from.
    ///
    /// `--tags 1.0.0,2.0.0 repo:9.9.9` resolves the sweep off `repo`, and the
    /// positional's own tag is never signed. Reporting it as the row's
    /// `identifier` puts a reference the run never touched next to a `tag`
    /// field that says otherwise. `subject_digest` was already correct, so
    /// nothing else in the row contradicts the wrong identifier — which is why
    /// this needs its own assertion.
    #[test]
    fn a_swept_row_reports_the_tag_it_signed_not_the_positional() {
        use ocx_lib::oci::sign::pipeline::{LegDigests, SignResult, SignatureLeg};

        let digest = |fill: char| oci::Digest::Sha256(fill.to_string().repeat(64));
        let positional = oci::Identifier::parse("registry.example/pkg:9.9.9").expect("static parse");
        let result = SignResult {
            subject_digest: digest('a'),
            legs: vec![SignatureLeg {
                format: ocx_lib::oci::sign::SignatureFormat::Bundle,
                outcome: Ok(LegDigests {
                    payload_digest: digest('b'),
                    manifest_digest: digest('c'),
                }),
            }],
            certificate_identity: "signer@example.com".into(),
            certificate_oidc_issuer: "https://accounts.google.com".into(),
            key_backend: ocx_lib::oci::sign::KeyBackendKind::Keyless,
            public_key_hint: None,
            transparency_log_index: None,
        };

        let report = swept_signature_report(&positional, "1.0.0", result);

        assert_eq!(
            report.identifier, "registry.example/pkg:1.0.0",
            "the row must name the swept tag, not the positional `9.9.9` the sweep never signed",
        );
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
        PackageSign::try_parse_from(argv).map(|_| ()).map_err(|e| e.kind())
    }

    #[test]
    fn every_keyless_only_flag_is_refused_alongside_a_key() {
        for flag in KEYLESS_ONLY_FLAGS {
            let mut argv = vec!["sign"];
            argv.extend_from_slice(&["--platform", "linux/amd64"]);
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
            let mut argv = vec!["sign"];
            argv.extend_from_slice(&["--platform", "linux/amd64"]);
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
        let mut argv = vec!["sign"];
        argv.extend_from_slice(&["--platform", "linux/amd64"]);
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
    //! `ocx package sign <ref>` into a usage error (64) before the pipeline ever
    //! ran, so no behavioural test further down could observe the widening.

    use super::*;

    /// The reference every case names, so each varies exactly one thing.
    const REFERENCE: &str = "registry.example/pkg:1.0";

    /// Every argument this command needs *besides* `--platform` and the
    /// reference.
    const REQUIRED: &[&str] = &["sign"];

    /// The parsed `--platform`, or clap's error kind on refusal. `extra` is
    /// appended after the required arguments.
    fn parse(extra: &[&str]) -> Result<Option<oci::Platform>, clap::error::ErrorKind> {
        let mut argv = REQUIRED.to_vec();
        argv.extend_from_slice(extra);
        PackageSign::try_parse_from(argv)
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
mod sweep_exclusivity_tests {
    //! `--platform` is refused alongside `--tags` and `--tags-file`.
    //!
    //! A sweep is about indices by definition; `--platform` narrows into one
    //! index to reach a child `push` already signed. Combining them asks for
    //! work that is either redundant or, on a tag resolving to a bare manifest,
    //! an error. The refusal is clap's, so these parse rather than execute.

    use super::*;

    /// The reference every case names, so each varies exactly one thing.
    const REFERENCE: &str = "registry.example/pkg:1.0";

    /// The two spellings of a sweep, each of which `--platform` must refuse.
    const SWEEP_FLAGS: &[&[&str]] = &[&["--tags", "3.28"], &["--tags-file", "tags.txt"]];

    fn parse(extra: &[&str]) -> Result<(), clap::error::ErrorKind> {
        let mut argv = vec!["sign"];
        argv.extend_from_slice(extra);
        argv.push(REFERENCE);
        PackageSign::try_parse_from(argv).map(|_| ()).map_err(|e| e.kind())
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

    /// The other half: each flag is perfectly legal on its own, so the test
    /// above measures the conflict rather than a broken command.
    #[test]
    fn each_flag_parses_on_its_own() {
        for sweep in SWEEP_FLAGS {
            assert_eq!(parse(sweep), Ok(()), "`{sweep:?}` must be legal without --platform");
        }
        assert_eq!(parse(&["--platform", "linux/amd64"]), Ok(()));
    }

    /// `--tags` and `--tags-file` are a union, not alternatives — they refuse
    /// `--platform`, never each other.
    #[test]
    fn the_two_sweep_flags_do_not_refuse_each_other() {
        assert_eq!(parse(&["--tags", "3.28", "--tags-file", "tags.txt"]), Ok(()));
    }
}
