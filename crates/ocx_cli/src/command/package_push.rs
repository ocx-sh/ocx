// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use ocx_lib::{
    log, oci, package,
    package::version::{BuildTimestampFormat, build_timestamp},
    publisher::{self, LayerRef, Publisher},
};

use crate::api::data::push::SignedPlatformReport;
use crate::command::package_sign_common;
use crate::options::key::KeyOpt;
use crate::options::rekor_upload::RekorUploadOpt;
use crate::options::signature_format::SignatureFormatOpt;
use crate::{conventions, options};

#[derive(Parser)]
// The three signing modifiers are inert without something to sign, and a flag
// that does nothing is the failure mode this spec rejects everywhere else. The
// refusal is clap's, not hand-written: `ArgGroup::requires` pointing at a
// second group renders "the following required arguments were not provided:
// <--sign|--sbom>", which names both flags that would make the modifier mean
// something. A hand-written check would have to reproduce that message, and it
// would need "was this flag given" accessors on three option groups that are
// shared with `sign`, `attest` and `verify` and deliberately expose only
// resolvers.
#[clap(group(clap::ArgGroup::new("signing_target").args(["sign", "sbom"]).multiple(true)))]
#[clap(group(
    clap::ArgGroup::new("signing_modifier")
        .args(["signature_format", "key", "rekor_upload", "no_rekor_upload", "fulcio_url", "rekor_url"])
        .multiple(true)
        .requires("signing_target")
))]
pub struct PackagePush {
    /// Will cascade rolling releases, ie. pushing 1.2.3 will also update 1.2, 1, etc.
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    /// Push a `__ocx.keep.sha256-<hex>` tag pointing at each pushed platform
    /// manifest (default). A stray delete of a rolling or cascade tag can then
    /// never orphan a digest something else still pins, since the keep tag
    /// names it directly. Pass `--no-keep-tag` to skip it.
    #[clap(flatten)]
    keep_tag: options::KeepTag,

    /// Append a UTC build-metadata segment to the published tag.
    ///
    /// `datetime` appends `_YYYYMMDDhhmmss`, `date` appends `_YYYYMMDD`,
    /// `none` is a no-op. Passing the flag without a value defaults to
    /// `datetime`. Must use `=` when supplying an explicit value
    /// (`--build-timestamp=date`); bare `--build-timestamp` with no `=`
    /// uses the `datetime` default. The version core in `--identifier`
    /// must already be `X.Y.Z` (optionally with variant prefix or
    /// pre-release); pushing against a tag that already carries build
    /// metadata is rejected.
    ///
    /// Use this in continuous-deploy pipelines to publish rolling versions
    /// like `dev.ocx.sh/ocx:0.3.0-dev_<YYYYMMDDhhmmss>`.
    #[clap(
        long = "build-timestamp",
        value_enum,
        num_args = 0..=1,
        default_missing_value = "datetime",
        require_equals = true,
    )]
    build_timestamp: Option<BuildTimestampFormat>,

    /// Path to the package metadata JSON file. Defaults to a sibling of the
    /// first file layer (e.g. `pkg.tar.gz` -> `pkg-metadata.json`). Required
    /// when no file layers are provided.
    ///
    /// This must be the compiled form `ocx package create --metadata` writes,
    /// with every dependency pinned to a digest; an authoring sidecar with
    /// tag-only dependencies is rejected. The build receipt is anchored to the
    /// bundle, so pointing this flag elsewhere does not move where an omitted
    /// `--platform` or `--identifier` is read from.
    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,

    /// Record an OCI annotation on the published image index. Repeatable.
    ///
    /// Written verbatim onto the index of every tag this push writes,
    /// including cascade tags. A repeated key keeps the last value. Omitting
    /// the flag writes no annotations and leaves any the registry already
    /// holds untouched.
    ///
    /// Set `org.opencontainers.image.source` to the HTTPS URL of the source
    /// repository: on GHCR this is what links the package to its repository
    /// and lets it inherit that repository's permissions. Registries derive
    /// nothing from the repository path, so state it explicitly - for example
    /// in GitHub Actions:
    ///
    ///   --annotation org.opencontainers.image.source=$GITHUB_SERVER_URL/$GITHUB_REPOSITORY
    #[clap(long = "annotation", value_name = "KEY=VALUE", value_parser = parse_annotation)]
    annotation: Vec<(String, String)>,

    /// After a successful push, append the pushed tag and any cascade tags
    /// to this file (creating it if absent), so `ocx package announce
    /// --tags-file` can pick them up.
    ///
    /// This is a scratch file for one pipeline run, not a persistent list -
    /// a stale file left over from an earlier run could re-add a tag that
    /// was deliberately dropped from a later announce.
    #[clap(long = "tags-file", value_name = "PATH")]
    tags_file: Option<std::path::PathBuf>,

    /// After the push, attest this CycloneDX SBOM against the pushed manifest.
    ///
    /// Sugar for `ocx package attest --type cyclonedx` on the digest this push
    /// just wrote. The file is read before the push, so a bad path costs no
    /// upload; the OIDC token comes from OCX_IDENTITY_TOKEN or from ambient
    /// CI detection, as `ocx package attest` resolves it.
    ///
    /// A push that lands followed by an attestation that fails is not rolled
    /// back: a pushed manifest is immutable and OCI offers no un-push. The
    /// push report is still emitted, with the attestation outcome recorded,
    /// and the attestation failure decides the exit code.
    #[clap(long = "sbom", value_name = "PATH")]
    sbom: Option<std::path::PathBuf>,

    /// Sign each platform manifest this push writes, inline.
    ///
    /// Opt-in: a push without it signs nothing. The signature covers the
    /// platform manifest, whose digest is final the moment it is pushed --
    /// never the image index, whose digest is rewritten every time another
    /// platform merges into it. Sign the index afterwards with
    /// `ocx package sign --tags-file`, using the file `--tags-file` wrote.
    ///
    /// Keyless by default; `--key` selects a key pair. A push that lands and
    /// then fails to sign is not rolled back: the push report is still
    /// emitted, with the per-platform signing outcome recorded, and the
    /// failure decides the exit code.
    #[clap(long = "sign")]
    sign: bool,

    /// Fulcio CA endpoint (the keyless certificate issuer)
    ///
    /// Defaults to [trust.sigstore].fulcio_url, else public Fulcio.
    ///
    /// Keyless-only: an error alongside `--key`, never silently ignored. A
    /// flag that does nothing is the failure mode this command refuses
    /// everywhere. A usage error without `--sign` or `--sbom`.
    #[clap(long = "fulcio-url", value_name = "URL", conflicts_with = "key")]
    fulcio_url: Option<String>,

    /// Rekor transparency-log endpoint
    ///
    /// Defaults to [trust.sigstore].rekor_url, else public Rekor. A usage
    /// error without `--sign` or `--sbom`.
    #[clap(long = "rekor-url", value_name = "URL")]
    rekor_url: Option<String>,

    /// Signature wire format for `--sign` and `--sbom`.
    ///
    /// A usage error without one of those two flags.
    #[clap(flatten)]
    signature_format: SignatureFormatOpt,

    /// Sign with a key pair instead of keyless Sigstore.
    ///
    /// A usage error without `--sign` or `--sbom`. The password for an
    /// encrypted private key is read from `OCX_KEY_PASSWORD`.
    #[clap(flatten)]
    key: KeyOpt,

    /// Whether the signature is recorded in the Rekor transparency log.
    ///
    /// A usage error without `--sign` or `--sbom`. Keyless signatures are
    /// always recorded and `--no-rekor-upload` is refused there; under `--key`
    /// recording is off unless asked for.
    #[clap(flatten)]
    rekor_upload: RekorUploadOpt,

    /// Target platform (e.g. `linux/amd64`, or `any` for platform-agnostic content)
    ///
    /// The pushed manifest is scoped to this platform. An explicit value is
    /// used as given. Omit it to take the platform the build receipt beside
    /// the bundle recorded; a usage error (exit 64) when neither names one.
    #[clap(short, long)]
    platform: Option<oci::Platform>,

    /// Identifier under which the package is published (e.g. `repo:2.0.0`)
    ///
    /// An explicit value is used as given. Omit it to take the identifier the
    /// build receipt beside the bundle recorded, which is what `ocx package
    /// create --identifier` wrote there; a usage error (exit 64) when neither
    /// names one. A value without a tag (`repo`) takes the version the
    /// receipt recorded when it names the same repository.
    #[clap(short = 'i', long = "identifier")]
    identifier: Option<options::Identifier>,

    /// Layers to push, in order (base layer first, top layer last).
    ///
    /// Each layer is either:
    ///   - a path to a pre-built archive file (`.tar.gz`, `.tar.xz`,
    ///     `.tar.zst`), or
    ///   - a digest reference to a layer already present in the target
    ///     registry, written as `sha256:<hex>.<ext>` where `<ext>` declares
    ///     the original archive format - one of `tar.gz`, `tgz`, `tar.xz`,
    ///     `txz`, `tar.zst`, `tzst`, `tar.zstd`. The OCI distribution spec
    ///     does not expose a layer's media type via blob HEAD, so the suffix
    ///     is required: OCX refuses to guess.
    ///
    /// Either form may carry an optional layout tail
    /// `:strip=N,prefix=P,from=REPO` that controls how the layer is placed
    /// when the package is installed and where it uploads from:
    ///   - `strip=N` drops the leading N path components (like
    ///     `tar --strip-components=N`).
    ///   - `prefix=P` relocates the layer under the relative subdirectory `P`
    ///     (must stay inside the package; `..`, absolute, and Windows-style
    ///     paths are rejected).
    ///   - `from=REPO` attempts a cross-repository blob mount from `REPO`
    ///     (same registry) before falling back to a normal upload. Use this
    ///     to reuse a layer already pushed to another repository without
    ///     re-uploading its bytes.
    ///
    /// All three keys are optional and comma-separated; omit the tail for
    /// the default (no strip, package root, no mount attempt).
    ///
    /// Digest references enable layer reuse: a base layer pushed once can be
    /// referenced by digest from many packages without re-uploading. Zero
    /// layers is valid (produces a config-only OCI artifact) when
    /// `--metadata` is supplied.
    ///
    /// Examples:
    ///   ocx package push repo:2.0.0 ./libs.tar.gz:strip=1,prefix=share
    ///   ocx package push repo:2.0.0 sha256:<hex>.tar.xz ./new.tar.zst
    ///   ocx package push app:1.0.0 ./layer.tar.gz:from=base-images/layer
    layers: Vec<LayerRef>,
}

impl PackagePush {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Read the build receipt only for what the command line left open: a
        // fully explicit push must not be able to fail on a file it never
        // needed. Resolved before `Publisher::new` and auth, so a push missing
        // both a flag and a recorded value fails on its own arguments without
        // a network round-trip first.
        let explicit_identifier = self
            .identifier
            .as_ref()
            .map(|identifier| identifier.with_domain(context.default_registry()))
            .transpose()?;
        // A tagless -i is not a complete answer: the receipt may still supply
        // the version, so it counts as a gap for the lazy read below.
        let identifier_answers = explicit_identifier
            .as_ref()
            .is_some_and(|identifier| identifier.tag().is_some() || identifier.digest().is_some());
        let receipt = if identifier_answers && self.platform.is_some() {
            None
        } else {
            crate::build_receipt::read_beside_bundle(&self.layers).await?
        };
        let identifier = crate::build_receipt::resolve_target_identifier(explicit_identifier, receipt.as_ref())?;
        // The published index entry's platform label must not decouple from
        // what the dependency pins were resolved against, which is why the
        // receipt is the fallback rather than the host platform.
        let platform = crate::build_receipt::resolve_target_platform(self.platform.clone(), receipt.as_ref())?;

        // `--sbom` work that must happen BEFORE the push, because a push is
        // not undoable. The offline refusal is first so it beats the generic
        // `OfflineMode` (81) `remote_client()` would raise: an offline attest
        // is a deliberate policy refusal (77) whichever verb reached it, and a
        // script branching on 77 must not see a different code here than it
        // sees from `ocx package attest`.
        //
        // WATCH: 77-before-81 is S-018's contract, pinned end to end in WP10a.
        // Moving this block below `Publisher::new` silently returns 81 instead
        // — no unit test here reaches `remote_client()`, so nothing local reds.
        if self.sign {
            package_sign_common::refuse_when_offline(
                &context,
                &identifier,
                ocx_lib::oci::sign::SignErrorKind::OfflineSignRefused,
            )?;
        }
        let sbom_predicate = match &self.sbom {
            None => None,
            Some(path) => {
                package_sign_common::refuse_when_offline(
                    &context,
                    &identifier,
                    ocx_lib::oci::sign::SignErrorKind::OfflineAttestRefused,
                )?;
                Some(package_sign_common::read_predicate(path, &identifier).await?)
            }
        };

        // Resolved before the push for the same reason the predicate is read
        // before it: a malformed `--key`, a keyless `--no-rekor-upload`, or a
        // forbidden `[trust.sigstore]` URL must cost no upload. Gated on a
        // signing request, because an ordinary push must not start failing on
        // a config key it never reads.
        let signing = match self.sign || self.sbom.is_some() {
            false => None,
            true => Some(self.resolve_signing(&context, &identifier).await?),
        };

        let metadata_path = conventions::resolve_metadata_path(&self.layers, self.metadata.as_deref())?;

        log::info!(
            "deploying package with {} layer(s) and metadata {}",
            self.layers.len(),
            metadata_path.display()
        );
        let metadata = conventions::read_published_metadata(&metadata_path).await?;

        // The publish gate, not the structural check: push is the last moment a
        // publisher can be told about an unrecognised token before it becomes a
        // published artifact nobody can edit (D14).
        let valid = package::metadata::validate_for_publish(metadata)?;

        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(&identifier).await?;

        // Gate: every dependency pin must name an existing platform MANIFEST
        // digest — push makes no resolution decisions (run `ocx package
        // create` for that).
        {
            let _spin = context.progress().spinner("Verifying dependency pins");
            publisher::verify_dependency_pins(publisher.client(), &valid, &platform).await?;
        }

        let infos = vec![package::info::Info {
            identifier: identifier.clone(),
            metadata: valid.into(),
            platform: platform.clone(),
        }];

        let build_meta: Option<String> = self.build_timestamp.as_ref().and_then(build_timestamp);
        let keep_tag = self.keep_tag.enabled();
        // Last-wins on a repeated key, matching the POSIX convention for
        // repeated flags.
        let annotations: BTreeMap<String, String> = self.annotation.iter().cloned().collect();

        let outcome = if self.cascade {
            let existing_tags = publisher
                .list_tags(identifier.clone())
                .await
                .with_context(|| format!("listing existing tags for {identifier}"))?;

            let existing_versions = Publisher::parse_versions(&existing_tags);
            publisher
                .push_cascade(
                    infos,
                    &self.layers,
                    existing_versions,
                    build_meta.as_deref(),
                    keep_tag,
                    &annotations,
                )
                .await?
        } else {
            publisher
                .push(infos, &self.layers, build_meta.as_deref(), keep_tag, &annotations)
                .await?
        };

        // The primary version tag plus the rolling cascade tags. The
        // `__ocx.keep.*` tags are deliberately left out: announce drops them
        // downstream, so recording one in a file named "announce" would state
        // something that never gets announced.
        let mut pushed_tags = vec![identifier.tag_or_latest().to_string()];
        pushed_tags.extend(outcome.cascade_tags.iter().cloned());

        // Emit the structured push report BEFORE the tags-file append. The
        // push itself already succeeded and is not undoable, so an I/O failure
        // writing the scratch file must not swallow the report — the caller
        // still has to learn what landed in the registry. Plain output is a
        // one-row table (identifier, digest, cascade + keep tags, layer
        // counts); `--format json`
        // serializes the report consumed by `ocx-mirror pipeline push`, and
        // adds the per-platform manifest digests, which are JSON-only because
        // the plain table is already at its five-column budget.
        // Read before the outcome is consumed: `platform_digests` is the
        // signing input, and it names the platform manifests -- never the
        // index, whose digest the next platform merge rewrites.
        let platform_digests = outcome.platform_digests.clone();
        let mut report = crate::api::data::push::PushReport::from_outcome(identifier.to_string(), outcome);

        // Post-push work is never rolled back -- a pushed manifest is
        // immutable and OCI offers no un-push -- so every failure below is a
        // row in the report and a line on stderr, and the process exit code is
        // `sweep_exit_code` over all of them: one fault class scripts through,
        // a mix collapses to the generic failure. Same collapse the `--tags`
        // sweep uses, and the same vocabulary in the rows.
        let mut failures: Vec<ocx_lib::cli::ExitCode> = Vec::new();

        if let Some(options) = &signing
            && self.sign
        {
            let signed = context
                .manager()
                .sign_platforms(&identifier, &platform_digests, options)
                .await;
            let mut rows = Vec::with_capacity(signed.len());
            for (platform, outcome) in signed {
                rows.push(match outcome {
                    Ok(signed) => {
                        // Read before the result is consumed, exactly as
                        // `sign` does: a `--signature-format both` platform
                        // that lost one leg is a failure that still carries
                        // the leg that landed.
                        let result = signed.result;
                        let leg = result
                            .first_failure()
                            .map(|kind| (package_sign_common::leg_exit_code(kind), kind.to_string()));
                        let signature = package_sign_common::signature_report(&identifier, Some(&platform), result);
                        match leg {
                            Some((code, message)) => {
                                failures.push(code);
                                SignedPlatformReport::failed(
                                    platform.to_string(),
                                    Some(signature),
                                    package_sign_common::category_slug(code),
                                    message,
                                )
                            }
                            None => SignedPlatformReport::completed(platform.to_string(), signature),
                        }
                    }
                    Err(error) => {
                        let error = package_sign_common::attest_error_into_anyhow(error);
                        failures.push(ocx_lib::cli::classify_error(error.as_ref()));
                        log::error!("{}", crate::api::data::sanitize_for_terminal(&format!("{error:#}")));
                        SignedPlatformReport::failed(
                            platform.to_string(),
                            None,
                            package_sign_common::error_slug("package push", &error),
                            format!("{error:#}"),
                        )
                    }
                });
            }
            report = report.with_signatures(rows);
        }

        // The push already landed. Whatever the attestation does, the report is
        // owed to the caller — so the outcome is folded into the report rather
        // than replacing it with an error envelope, and the error is returned
        // only after the report is on stdout.
        if let Some(predicate) = sbom_predicate {
            let options = signing.expect("--sbom resolves the signing options above");
            match Self::attest_sbom(&context, &identifier, &platform, options, predicate).await {
                Ok(outcome) => report = report.with_attestation(outcome),
                Err(err) => {
                    report = report.with_attestation(package_sign_common::failed_outcome(&err));
                    failures.push(ocx_lib::cli::classify_error(err.as_ref()));
                    log::error!("{}", crate::api::data::sanitize_for_terminal(&format!("{err:#}")));
                }
            }
        }
        context.api().report(&report)?;

        // The append still decides the exit code: the caller asked for the file,
        // so a failure is a failure — it just no longer costs them the report.
        if let Some(path) = &self.tags_file
            && let Err(error) = append_to_tags_file(path, &pushed_tags).await
        {
            context.ui().warn(format!(
                "the push succeeded but the tags file {} was not written",
                path.display()
            ));
            return Err(error);
        }

        // The push succeeded, so a post-push failure is the worst outcome in
        // the run and owns the exit code. It is returned rather than raised:
        // the push report already claimed stdout, so an error envelope would
        // be suppressed anyway, and only a code can express the sweep's
        // "a mixed set collapses to Failure" rule. Each failure was logged
        // above, which is the line `main` would have printed for a raised one.
        Ok(ExitCode::from(package_sign_common::sweep_exit_code(&failures)))
    }

    /// Resolve the one option set both `--sign` and `--sbom` sign under.
    ///
    /// [`SignOptions`] is the carrier rather than a second struct: it already
    /// holds every field [`AttestOptions`] needs beyond the predicate, and
    /// minting a parallel type would give the Rekor-upload asymmetry a second
    /// place to drift.
    ///
    /// `--fulcio-url` and `--rekor-url` let both URLs enter the shared ladder
    /// as an explicit flag, ahead of `[trust.sigstore]` and the builtin
    /// default, behind the same SSRF guard `sign` uses. `no_tty` is `false`
    /// and the token overrides are absent, matching what `push --sbom`
    /// already did.
    ///
    /// # Errors
    ///
    /// A forbidden endpoint URL, a malformed `--key` reference (exit 64) or a
    /// recognised-but-unimplemented key backend (exit 85), and the keyless
    /// `--no-rekor-upload` refusal (exit 64). Each carries the identifier,
    /// because each is returned as a `SignError` rather than a bare kind.
    ///
    /// [`AttestOptions`]: ocx_lib::package_manager::AttestOptions
    async fn resolve_signing(
        &self,
        context: &crate::app::Context,
        identifier: &oci::Identifier,
    ) -> anyhow::Result<ocx_lib::package_manager::SignOptions> {
        let (fulcio_url, rekor_url) = package_sign_common::resolve_sigstore_pair(
            context.config_trust_sigstore(),
            identifier,
            self.fulcio_url.as_deref(),
            self.rekor_url.as_deref(),
        )?;
        // Both refusals are wrapped in `SignError` before they reach `anyhow`:
        // `classify_error` downcasts the outer error, so a bare
        // `SignErrorKind` exits 1 with an empty `context` instead of 85/64
        // with the identifier. `sign` and `attest` wrap at the same two calls.
        let key = self.key.reference().map_err(|kind| {
            ocx_lib::oci::sign::SignError::new(identifier.clone(), ocx_lib::oci::sign::SignErrorKind::from(kind))
        })?;
        let configured_rekor_upload = context
            .config_trust_sigstore()
            .and_then(|sigstore| sigstore.rekor_upload);
        let rekor_upload = self
            .rekor_upload
            .enabled(self.key.is_key_mode(), configured_rekor_upload)
            .map_err(|kind| ocx_lib::oci::sign::SignError::new(identifier.clone(), kind))?;
        // The OIDC token comes from OCX_IDENTITY_TOKEN or ambient CI detection
        // exactly as `ocx package attest` resolves it; `push` carries no
        // `--identity-token-*` flags, so both overrides enter as absent.
        let identity_token = package_sign_common::resolve_override_token(None, false, identifier).await?;
        Ok(ocx_lib::package_manager::SignOptions {
            fulcio_url,
            rekor_url,
            identity_token,
            no_cache: false,
            no_tty: false,
            key,
            rekor_upload,
            format: self.signature_format.write_format(),
        })
    }

    /// Attest `predicate` as a CycloneDX SBOM against the manifest this push
    /// wrote for `platform`.
    ///
    /// The subject digest is resolved by the attest pipeline from the
    /// identifier and platform, never derived from a keep tag —
    /// `--no-keep-tag` may have suppressed those.
    ///
    /// `options` is the same [`SignOptions`](ocx_lib::package_manager::SignOptions)
    /// the inline platform signing ran under, so `--signature-format`, `--key`
    /// and `--rekor-upload` mean one thing per invocation. `push --sbom` used
    /// to hard-code keyless with a mandatory Rekor upload because push carried
    /// none of those flags; it carries them now.
    ///
    /// # Errors
    ///
    /// Any attest-pipeline failure, already re-rooted so the JSON envelope
    /// keeps its `context.identifier`.
    async fn attest_sbom(
        context: &crate::app::Context,
        identifier: &oci::Identifier,
        platform: &oci::Platform,
        options: ocx_lib::package_manager::SignOptions,
        predicate: Vec<u8>,
    ) -> anyhow::Result<crate::api::data::push::AttestationOutcome> {
        use ocx_lib::oci::attest::predicate::PredicateType;

        let result = context
            .manager()
            .attest_one(
                identifier,
                Some(platform),
                ocx_lib::package_manager::AttestOptions {
                    key: options.key,
                    rekor_upload: options.rekor_upload,
                    format: options.format,
                    fulcio_url: options.fulcio_url,
                    rekor_url: options.rekor_url,
                    identity_token: options.identity_token,
                    predicate_type: PredicateType::CycloneDx,
                    predicate,
                    no_cache: options.no_cache,
                    no_tty: options.no_tty,
                    offline: context.is_offline(),
                },
                None,
            )
            .await
            .map_err(package_sign_common::attest_error_into_anyhow)?
            .result;
        // Both addresses are reported, and neither is required: under
        // `--signature-format simplesigning` the pipeline writes the
        // `sha256-<hex>.att` sidecar and no referrer, which is a published
        // attestation and not a failure. `SignatureFormat` has no variant that
        // writes nothing, so at least one of the two is `Some`.
        Ok(crate::api::data::push::AttestationOutcome::Succeeded {
            referrer_digest: result.referrer.map(|leg| leg.manifest_digest.to_string()),
            sidecar_digest: result.sidecar.map(|leg| leg.manifest_digest.to_string()),
            predicate_type: result.predicate_type,
            signed: result.signed,
        })
    }
}

/// Splits a `KEY=VALUE` annotation argument at the first `=`.
///
/// The value may contain `=` (URLs with query strings do) and may be empty;
/// the key may not be empty. Beyond that OCX does not police annotation keys —
/// the OCI spec only *recommends* reverse-domain notation, and a registry is
/// free to define its own.
pub(crate) fn parse_annotation(argument: &str) -> anyhow::Result<(String, String)> {
    let (key, value) = argument
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected KEY=VALUE, got '{argument}'"))?;
    if key.is_empty() {
        return Err(anyhow::anyhow!("annotation key is empty in '{argument}'"));
    }
    Ok((key.to_string(), value.to_string()))
}

/// Appends `tags` onto the tags-file at `path` (created if absent),
/// deduping against whatever is already there (design register C2).
async fn append_to_tags_file(path: &std::path::Path, tags: &[String]) -> anyhow::Result<()> {
    // The shared bounded reader, not a bare `fs::read`: this path is
    // operator-typed, so `--tags-file /dev/zero` read until memory ran out.
    // Absence is still not a failure — the file is created below.
    let existing = crate::options::tags::read_tags_file_if_present(path).await?;
    let merged = conventions::merge_tags_file(&existing, tags);
    tokio::fs::write(path, merged)
        .await
        .map_err(|error| ocx_lib::error::file_error(path, error))
        .with_context(|| format!("writing tags file {}", path.display()))
}

#[cfg(test)]
mod stderr_neutralization_tests {
    //! `push` is the one command that logs a failure *and* keeps going: the
    //! push already landed, so an inline-signing or attestation failure is
    //! reported rather than returned, and `main.rs`'s boundary log — the only
    //! other place a cause chain reaches the terminal — never sees these
    //! chains at all. That makes each of these sites a terminal boundary in
    //! its own right (CWE-150): a registry-served error body quotes names read
    //! off wire documents, and `tracing-subscriber` passes `\n`, `\r`, NUL and
    //! the whole `Cf` bidi set straight through.

    /// Every `log::error!` in this module's production half neutralizes what
    /// it interpolates.
    ///
    /// Written per call site rather than as a count budget: comparing totals
    /// (`log::error!` count == `sanitize_for_terminal` count) is satisfied by
    /// one raw call plus one unrelated sanitized call elsewhere in the file,
    /// which is exactly the shape that shipped. The non-zero assertion is the
    /// other half — a needle that silently stops matching still reports green.
    #[test]
    fn every_stderr_log_neutralizes_its_cause_chain() {
        let production = include_str!("package_push.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half");
        let sites: Vec<&str> = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains("log::error!"))
            .collect();
        assert!(
            !sites.is_empty(),
            "the needle stopped matching — this guard would now pass on any code at all"
        );
        for site in sites {
            assert!(
                site.contains("sanitize_for_terminal"),
                "an unsanitized cause chain reaches the terminal here: {}",
                site.trim()
            );
        }
    }
}

#[cfg(test)]
mod annotation_tests {
    use std::collections::BTreeMap;

    use super::parse_annotation;

    #[test]
    fn splits_at_the_first_equals_sign() {
        let (key, value) =
            parse_annotation("org.opencontainers.image.source=https://github.com/ocx-sh/ocx?ref=main").expect("parses");
        assert_eq!(key, "org.opencontainers.image.source");
        assert_eq!(value, "https://github.com/ocx-sh/ocx?ref=main");
    }

    #[test]
    fn accepts_an_empty_value() {
        let (key, value) = parse_annotation("org.opencontainers.image.source=").expect("parses");
        assert_eq!(key, "org.opencontainers.image.source");
        assert_eq!(value, "");
    }

    #[test]
    fn rejects_an_argument_without_an_equals_sign() {
        let error = parse_annotation("org.opencontainers.image.source").expect_err("must reject");
        assert!(error.to_string().contains("expected KEY=VALUE"), "got: {error}");
    }

    #[test]
    fn rejects_an_empty_key() {
        let error = parse_annotation("=https://github.com/ocx-sh/ocx").expect_err("must reject");
        assert!(error.to_string().contains("annotation key is empty"), "got: {error}");
    }

    #[test]
    fn a_repeated_key_keeps_the_last_value() {
        let parsed = ["a=first", "b=x", "a=second"].map(|argument| parse_annotation(argument).expect("parses"));
        let collected: BTreeMap<String, String> = parsed.into_iter().collect();
        assert_eq!(collected.get("a").map(String::as_str), Some("second"));
        assert_eq!(collected.get("b").map(String::as_str), Some("x"));
    }
}

#[cfg(test)]
mod tags_file_tests {
    use super::append_to_tags_file;

    #[tokio::test]
    async fn creates_the_file_with_the_pushed_and_cascade_tags() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("announce.txt");

        append_to_tags_file(
            &path,
            &[
                "3.28.1".to_string(),
                "3.28".to_string(),
                "3".to_string(),
                "latest".to_string(),
            ],
        )
        .await
        .expect("append succeeds");

        let content = tokio::fs::read_to_string(&path).await.expect("read announce file");
        assert_eq!(content, "3.28.1,3.28,3,latest");
    }

    #[tokio::test]
    async fn a_second_overlapping_append_dedupes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("announce.txt");

        append_to_tags_file(&path, &["3.28.1".to_string(), "3.28".to_string()])
            .await
            .expect("first append succeeds");
        append_to_tags_file(&path, &["3.28.2".to_string(), "3.28".to_string()])
            .await
            .expect("second append succeeds");

        let content = tokio::fs::read_to_string(&path).await.expect("read announce file");
        assert_eq!(content, "3.28.1,3.28,3.28.2");
    }
}

#[cfg(test)]
mod signing_flag_tests {
    //! The cross-flag rule: `--signature-format`, `--key` and `--rekor-upload`
    //! are inert without something to sign, so clap refuses them without
    //! `--sign` or `--sbom`.
    //!
    //! The refusal is expressed as an `ArgGroup` whose `requires` names a
    //! second group, never as a hand-written check: clap then renders
    //! `<--sign|--sbom <PATH>>`, which names both flags that would give the
    //! modifier a meaning, and the three option groups stay untouched — they
    //! are shared with `sign`, `attest` and `verify` and deliberately expose
    //! resolvers rather than "was this given" predicates.

    use clap::Parser as _;
    use ocx_lib::oci::sign::SignatureFormat;

    use super::PackagePush;

    /// Every modifier flag, spelled as argv.
    ///
    /// OCX-C-5 added the two endpoint flags, and they join this list rather
    /// than getting a test of their own: both group rules below iterate it, so
    /// a member added here is covered by the refusal case *and* the
    /// both-targets case at once — which is exactly the pair a flag admitted
    /// by only one of them would slip through.
    const MODIFIERS: [&[&str]; 6] = [
        &["--signature-format", "bundle"],
        &["--key", "/tmp/does-not-need-to-exist.pem"],
        &["--rekor-upload"],
        &["--no-rekor-upload"],
        &["--fulcio-url", "https://f.example"],
        &["--rekor-url", "https://r.example"],
    ];

    fn parse(extra: &[&str]) -> Result<PackagePush, clap::Error> {
        let mut argv = vec!["push"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["--identifier", "registry.example/pkg:1.0"]);
        PackagePush::try_parse_from(argv)
    }

    /// Without a target, each modifier is a usage error naming both flags that
    /// would make it mean something.
    #[test]
    fn a_signing_modifier_alone_is_refused_and_names_the_flags_that_would_admit_it() {
        for modifier in MODIFIERS {
            let Err(error) = parse(modifier) else {
                panic!("{modifier:?} must be refused without a target");
            };
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument,
                "{modifier:?} must be a missing-requirement refusal, not a parse failure"
            );
            let rendered = error.to_string();
            assert!(
                rendered.contains("--sign") && rendered.contains("--sbom"),
                "{modifier:?} must name both admitting flags, got: {rendered}"
            );
        }
    }

    /// Either target admits every modifier — `--sbom` included, which is the
    /// half a `requires = "sign"` would have got wrong.
    #[test]
    fn either_target_admits_every_signing_modifier() {
        for modifier in MODIFIERS {
            for target in [vec!["--sign"], vec!["--sbom", "/tmp/sbom.json"]] {
                let mut argv = target.clone();
                argv.extend_from_slice(modifier);
                parse(&argv).unwrap_or_else(|error| panic!("{target:?} must admit {modifier:?}: {error}"));
            }
        }
    }

    /// A bare push, and a `--sign` with no modifiers, both still parse: the
    /// group is a requirement on the modifiers, never on the targets.
    #[test]
    fn a_push_parses_with_no_signing_flags_at_all() {
        parse(&[]).expect("a bare push parses");
        parse(&["--sign"]).expect("--sign alone parses");
    }

    /// The flattened groups are wired to push's own fields, so the resolvers
    /// read what the command line said rather than their defaults.
    #[test]
    fn the_flattened_groups_reach_pushs_resolvers() {
        let push = parse(&["--sign", "--key", "/tmp/k.pem", "--signature-format", "both"]).expect("parses");
        assert!(push.key.is_key_mode(), "--key must select key mode");
        assert_eq!(push.signature_format.write_format(), SignatureFormat::Both);
        assert!(
            !push.rekor_upload.enabled(true, None).expect("key mode resolves"),
            "key mode does not upload unless asked"
        );

        let opted_in = parse(&["--sign", "--key", "/tmp/k.pem", "--rekor-upload"]).expect("parses");
        assert!(opted_in.rekor_upload.enabled(true, None).expect("key mode resolves"));

        let default_format = parse(&["--sign"]).expect("parses");
        assert_eq!(default_format.signature_format.write_format(), SignatureFormat::Bundle);
        assert!(!default_format.key.is_key_mode());
    }

    /// Keyless plus `--no-rekor-upload` is refused on push exactly as on
    /// `sign`: a Fulcio certificate outlives its validity window, and the log
    /// entry's timestamp is the only lasting proof the signature predates the
    /// expiry.
    #[test]
    fn keyless_with_no_rekor_upload_is_refused_on_push_too() {
        let push = parse(&["--sign", "--no-rekor-upload"]).expect("parses");
        let error = push
            .rekor_upload
            .enabled(push.key.is_key_mode(), None)
            .expect_err("keyless must refuse --no-rekor-upload");
        assert!(
            matches!(error, ocx_lib::oci::sign::SignErrorKind::RekorUploadRequiredForKeyless),
            "got: {error}"
        );
    }

    // ── OCX-C-5: the two endpoint flags ────────────────────────────────

    /// **OCX-C-5.** `--fulcio-url` is keyless-only and conflicts with `--key`;
    /// `--rekor-url` is allowed in key mode alongside `--rekor-upload`.
    ///
    /// The asymmetry is the contract, so both halves are asserted here: a
    /// conflict on both flags would break the key-mode operator who runs a
    /// self-hosted Rekor, and a conflict on neither would let `--fulcio-url`
    /// pass silently into a run that never contacts Fulcio — the
    /// flag-that-does-nothing failure this command refuses everywhere.
    #[test]
    fn fulcio_url_is_keyless_only_while_rekor_url_is_allowed_under_a_key() {
        // `let ... else` rather than `expect_err`: `PackagePush` carries a key
        // reference and deliberately implements no `Debug`, so the `Ok` half
        // cannot be formatted into a panic message.
        let Err(error) = parse(&["--sign", "--fulcio-url", "https://f.example", "--key", "/tmp/k.pem"]) else {
            panic!("--fulcio-url names a certificate issuer a key-mode signature never asks");
        };
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "the refusal must be clap's conflict, not a parse failure: {error}"
        );

        parse(&[
            "--sign",
            "--key",
            "/tmp/k.pem",
            "--rekor-upload",
            "--rekor-url",
            "https://r.example",
        ])
        .expect("a key-mode signature may name the transparency log it is recorded in");
    }

    /// **OCX-C-5.** The endpoint pair `push` resolves prefers its own flags
    /// over `[trust.sigstore]`.
    ///
    /// Both tiers are populated and they disagree, so a resolver reading the
    /// wrong one is a wrong *value* rather than an absent one — which a
    /// flag-only or config-only fixture could not tell apart. The flags are
    /// read off the parsed command exactly as `resolve_signing` reads them, so
    /// a flag that never reached the struct reds this too.
    ///
    /// `[trust.sigstore]` is what makes a self-hosted stack a fleet-wide
    /// setting; the flags are what let one publish run out-vote it, which is
    /// the whole reason ocx-mirror needs them (D1).
    #[test]
    fn the_resolved_endpoint_pair_prefers_pushs_flags_over_trust_sigstore() {
        let configured = ocx_lib::trust::SigstoreTrust {
            fulcio_url: Some("https://fleet-fulcio.example".to_string()),
            rekor_url: Some("https://fleet-rekor.example".to_string()),
            ..ocx_lib::trust::SigstoreTrust::default()
        };
        let identifier = ocx_lib::oci::Identifier::parse("registry.example/pkg:1.0").expect("static parse");

        let push = parse(&[
            "--sign",
            "--fulcio-url",
            "https://flag-fulcio.example",
            "--rekor-url",
            "https://flag-rekor.example",
        ])
        .expect("both endpoint flags parse under --sign");

        let (fulcio, rekor) = crate::command::package_sign_common::resolve_sigstore_pair(
            Some(&configured),
            &identifier,
            push.fulcio_url.as_deref(),
            push.rekor_url.as_deref(),
        )
        .expect("both flag values are valid https endpoints");

        assert_eq!(fulcio.as_str().trim_end_matches('/'), "https://flag-fulcio.example");
        assert_eq!(rekor.as_str().trim_end_matches('/'), "https://flag-rekor.example");
    }
}
