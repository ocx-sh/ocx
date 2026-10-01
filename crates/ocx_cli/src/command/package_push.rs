// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_oci::layer_ref::LayerRef;
use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use ocx_package::{
    publisher::{self, Publisher},
    version::{BuildTimestampFormat, Version, build_timestamp},
};

use crate::api::data::push::SignedPlatformReport;
use crate::command::package_sign_common;
use crate::options::key::KeyOpt;
use crate::options::rekor_upload::RekorUploadOpt;
use crate::options::signature_format::SignatureFormatOpt;
use crate::{conventions, options};

const KEEP_TAG_LONG_HELP: &str = "\
Write a `__ocx.keep.sha256-<hex>` tag for each platform manifest published (default).\n\n\
The keep tag names the digest directly, so a stray delete of a rolling or cascade tag can never \
orphan a manifest something else still pins. It also pins the manifest against pruning: push \
ephemeral builds with `--no-keep-tag`, or prune frees no storage.";

#[derive(Parser)]
// The shared flag's help is generic; only a push knows its builds may be ephemeral.
#[clap(mut_arg("keep_tag", |arg| arg.long_help(KEEP_TAG_LONG_HELP)))]
// `requires` refuses a signing modifier without `--sign`/`--sbom`, which would otherwise be a silent no-op.
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

    /// Let the pushed tag's variant also own the un-prefixed version track
    #[arg(long_help = "\
        Let the pushed tag's variant also own the un-prefixed version track\n\n\
        A package whose every build is a named variant (`full-1.2.3`, `slim-1.2.3`) publishes no \
        bare `1.2.3`, so `ocx package install <ref>` with no variant resolves nothing. Pushing a \
        named variant with this flag additionally tags the same manifest under the version with the \
        prefix stripped - one upload, two tag sets.\n\n\
        With `--cascade` the bare track cascades too (`1.2.3`, `1.2`, `1`, `latest`), blocked by \
        newer bare versions exactly as any other track is; without it only the bare version is \
        written. Only the index of each alias tag is written - no blob and no manifest is uploaded \
        twice.\n\n\
        The pushed tag must carry a variant; `--default` on a tag with no variant is a usage error. \
        In a pipeline that pushes every variant, pass this flag only on the build whose variant \
        should own the bare track.")]
    #[clap(long = "default")]
    default: bool,

    #[clap(flatten)]
    keep_tag: options::KeepTag,

    /// Append a UTC build-metadata segment to the published tag.
    #[arg(long_help = "\
        Append a UTC build-metadata segment to the published tag.\n\n\
        `datetime` appends `_YYYYMMDDhhmmss`, `date` appends `_YYYYMMDD`, `none` is a no-op. \
        Passing the flag without a value defaults to `datetime`. Must use `=` when supplying an \
        explicit value (`--build-timestamp=date`); bare `--build-timestamp` with no `=` uses the \
        `datetime` default. The version core in `--identifier` must already be `X.Y.Z` (optionally \
        with variant prefix or pre-release); pushing against a tag that already carries build \
        metadata is rejected.\n\n\
        Use this in continuous-deploy pipelines to publish rolling versions like \
        `dev.ocx.sh/ocx:0.3.0-dev_<YYYYMMDDhhmmss>`.")]
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
    #[arg(long_help = "\
        Record an OCI annotation on the published image index. Repeatable.\n\n\
        Written verbatim onto the index of every tag this push writes, including cascade tags. A \
        repeated key keeps the last value. Omitting the flag writes no annotations and leaves any \
        the registry already holds untouched.\n\n\
        Set `org.opencontainers.image.source` to the HTTPS URL of the source repository: on GHCR \
        this is what links the package to its repository and lets it inherit that repository's \
        permissions. Registries derive nothing from the repository path, so state it explicitly - \
        for example in GitHub Actions:\n\n\
        --annotation org.opencontainers.image.source=$GITHUB_SERVER_URL/$GITHUB_REPOSITORY")]
    #[clap(long = "annotation", value_name = "KEY=VALUE", value_parser = parse_annotation)]
    annotation: Vec<(String, String)>,

    /// Stamp OCI annotations from the CI environment
    #[arg(long_help = "\
        Stamp OCI annotations from the CI environment\n\n\
        Records `org.opencontainers.image.source`, `.revision`, `.created` and `.version` on every \
        index this push writes, cascade tags included. `--ci-annotations=github` reads \
        `$GITHUB_SERVER_URL`, `$GITHUB_REPOSITORY` and `$GITHUB_SHA`; `--ci-annotations=gitlab` \
        reads `$CI_PROJECT_URL`, `$CI_COMMIT_SHA` and `$CI_PIPELINE_CREATED_AT`. \
        `$SOURCE_DATE_EPOCH`, when set, decides `.created` on either.\n\n\
        Bare `--ci-annotations` autodetects the provider from the environment; a usage error (exit \
        64) when none is detected. Must be supplied with `=` (`--ci-annotations=gitlab`).\n\n\
        `.version` is the value a pipeline cannot assemble for itself: with `--identifier` omitted \
        the tag comes from the build receipt, and this push is what resolves it. The variant prefix \
        is stripped, so pushing `full-1.2.3` annotates `1.2.3`; a tag that is not a version \
        annotates no version.\n\n\
        A variable that is unset or blank writes no annotation rather than an empty one, and an \
        explicit `--annotation` on the same key wins.")]
    #[clap(
        long = "ci-annotations",
        value_enum,
        value_name = "PROVIDER",
        num_args = 0..=1,
        require_equals = true,
    )]
    ci_annotations: Option<Option<ocx_shell::ci::CiFlavor>>,

    /// After a successful push, append the pushed tag and any cascade tags
    /// to this file, one per line (creating it if absent), so `ocx package
    /// announce --tags-file` can pick them up.
    ///
    /// This is a scratch file for one pipeline run, not a persistent list -
    /// a stale file left over from an earlier run could re-add a tag that
    /// was deliberately dropped from a later announce.
    #[clap(long = "tags-file", value_name = "PATH")]
    tags_file: Option<std::path::PathBuf>,

    /// After the push, attest this CycloneDX SBOM against the pushed manifest.
    #[arg(long_help = "\
        After the push, attest this CycloneDX SBOM against the pushed manifest.\n\n\
        Sugar for `ocx package attest --type cyclonedx` on the digest this push just wrote. The \
        file is read before the push, so a bad path costs no upload; the OIDC token comes from \
        OCX_IDENTITY_TOKEN or from ambient CI detection, as `ocx package attest` resolves it.\n\n\
        A push that lands followed by an attestation that fails is not rolled back: a pushed \
        manifest is immutable and OCI offers no un-push. The push report is still emitted, with the \
        attestation outcome recorded, and the attestation failure decides the exit code.")]
    #[clap(long = "sbom", value_name = "PATH")]
    sbom: Option<std::path::PathBuf>,

    /// Sign each platform manifest this push writes, inline.
    #[arg(long_help = "\
        Sign each platform manifest this push writes, inline.\n\n\
        Opt-in: a push without it signs nothing. The signature covers the platform manifest, whose \
        digest is final the moment it is pushed -- never the image index, whose digest is rewritten \
        every time another platform merges into it. Sign the index afterwards with `ocx package \
        sign --tags-file`, using the file `--tags-file` wrote.\n\n\
        Keyless by default; `--key` selects a key pair. A push that lands and then fails to sign is \
        not rolled back: the push report is still emitted, with the per-platform signing outcome \
        recorded, and the failure decides the exit code.")]
    #[clap(long = "sign")]
    sign: bool,

    /// Fulcio CA endpoint (the keyless certificate issuer)
    ///
    /// Defaults to [trust.sigstore].fulcio_url, else public Fulcio.
    ///
    /// Keyless-only: an error alongside `--key`, never silently ignored. A
    /// usage error without `--sign` or `--sbom`.
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
    platform: Option<ocx_oci::Platform>,

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
    #[arg(long_help = "\
        Layers to push, in order (base layer first, top layer last).\n\n\
        Each layer is either: - a path to a pre-built archive file (`.tar.gz`, `.tar.xz`, \
        `.tar.zst`), or - a digest reference to a layer already present in the target registry, \
        written as `sha256:<hex>.<ext>` where `<ext>` declares the original archive format - one of \
        `tar.gz`, `tgz`, `tar.xz`, `txz`, `tar.zst`, `tzst`, `tar.zstd`. The OCI distribution spec \
        does not expose a layer's media type via blob HEAD, so the suffix is required: OCX refuses \
        to guess.\n\n\
        Either form may carry an optional layout tail `:strip=N,prefix=P,from=REPO` that controls \
        how the layer is placed when the package is installed and where it uploads from: - \
        `strip=N` drops the leading N path components (like `tar --strip-components=N`). - \
        `prefix=P` relocates the layer under the relative subdirectory `P` (must stay inside the \
        package; `..`, absolute, and Windows-style paths are rejected). - `from=REPO` attempts a \
        cross-repository blob mount from `REPO` (same registry) before falling back to a normal \
        upload. Use this to reuse a layer already pushed to another repository without re-uploading \
        its bytes.\n\n\
        All three keys are optional and comma-separated; omit the tail for the default (no strip, \
        package root, no mount attempt).\n\n\
        Digest references enable layer reuse: a base layer pushed once can be referenced by digest \
        from many packages without re-uploading. Zero layers is valid (produces a config-only OCI \
        artifact) when `--metadata` is supplied.\n\n\
        Examples: ocx package push repo:2.0.0 ./libs.tar.gz:strip=1,prefix=share ocx package push \
        repo:2.0.0 sha256:<hex>.tar.xz ./new.tar.zst ocx package push app:1.0.0 \
        ./layer.tar.gz:from=base-images/layer")]
    layers: Vec<LayerRef>,
}

impl PackagePush {
    /// Refuses `--ci-annotations gitlab`, which `require_equals` reads as a bare flag plus a layer
    /// named `gitlab`.
    fn refuse_spaced_ci_annotations(&self) -> Result<(), crate::error::UsageError> {
        conventions::refuse_spaced_enum_value::<ocx_shell::ci::CiFlavor>(
            "--ci-annotations",
            matches!(self.ci_annotations, Some(None)),
            self.layers.iter().map(ToString::to_string),
        )
    }

    /// Refuses `--build-timestamp none`, which `require_equals` reads as a bare flag (`Datetime`)
    /// plus a layer named `none`, silently publishing a timestamp.
    fn refuse_spaced_build_timestamp(&self) -> Result<(), crate::error::UsageError> {
        // Sound only while `default_missing_value` is `datetime`
        // (`a_bare_build_timestamp_still_resolves_to_datetime`).
        let could_be_bare = matches!(self.build_timestamp, Some(BuildTimestampFormat::Datetime));
        conventions::refuse_spaced_enum_value::<BuildTimestampFormat>(
            "--build-timestamp",
            could_be_bare,
            self.layers.iter().map(ToString::to_string),
        )
    }

    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Before the autodetect, or a spaced value is silently ignored in CI and blamed on the
        // environment outside it.
        self.refuse_spaced_ci_annotations()?;
        self.refuse_spaced_build_timestamp()?;
        let ci_annotations = conventions::resolve_ci_annotations_arg(self.ci_annotations)?;

        // Read only for what the command line left open: a fully explicit push must not fail on a file it never needed.
        let explicit_identifier = self
            .identifier
            .as_ref()
            .map(|identifier| identifier.with_domain(context.default_registry()))
            .transpose()?;
        // A tagless `-i` still leaves the version to the receipt.
        let identifier_answers = explicit_identifier
            .as_ref()
            .is_some_and(|identifier| identifier.tag().is_some() || identifier.digest().is_some());
        let receipt = if identifier_answers && self.platform.is_some() {
            None
        } else {
            crate::build_receipt::read_beside_bundle(&self.layers).await?
        };
        let identifier = crate::build_receipt::resolve_target_identifier(explicit_identifier, receipt.as_ref())?;
        // Falls back to the receipt, never the host, or the index label decouples from what the pins resolved against.
        let platform = crate::build_receipt::resolve_target_platform(self.platform.clone(), receipt.as_ref())?;

        if self.default && Version::parse(identifier.tag_or_latest()).is_none_or(|version| version.variant().is_none())
        {
            return Err(crate::error::UsageError::new(format!(
                "--default requires a variant-prefixed tag, but {} carries no variant",
                identifier.tag_or_latest()
            ))
            .into());
        }

        // Before `Publisher::new`, or offline exits 81 instead of 77 (no unit test reaches `remote_client()`).
        if self.sign {
            package_sign_common::refuse_when_offline(
                &context,
                &identifier,
                ocx_sign::sign::SignErrorKind::OfflineSignRefused,
            )?;
        }
        let sbom_predicate = match &self.sbom {
            None => None,
            Some(path) => {
                package_sign_common::refuse_when_offline(
                    &context,
                    &identifier,
                    ocx_sign::sign::SignErrorKind::OfflineAttestRefused,
                )?;
                Some(package_sign_common::read_predicate(path, &identifier).await?)
            }
        };

        // Before the push, so a bad `--key`, keyless `--no-rekor-upload` or forbidden URL costs no upload.
        // Gated on a signing request, or an ordinary push fails on a `[trust.sigstore]` key it never reads.
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

        // The publish gate, not the structural check: past this, an unrecognised token is published immutably.
        let valid = ocx_package::metadata::validate_for_publish(metadata)?;

        // `passthrough`, never index-routed: a write goes where the identifier names.
        let target = ocx_oci::OciIdentifier::passthrough(&identifier);
        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(&target).await?;

        {
            let _spin = context.progress().spinner("Verifying dependency pins");
            publisher::verify_dependency_pins(publisher.client(), context.default_index(), &valid, &platform).await?;
        }

        let infos = vec![ocx_package::info::Info {
            metadata: valid.into(),
            platform: platform.clone(),
        }];

        let build_meta: Option<String> = self.build_timestamp.as_ref().and_then(build_timestamp);
        let keep_tag = self.keep_tag.enabled();
        let generated = match ci_annotations {
            None => BTreeMap::new(),
            Some(flavor) => ocx_shell::ci::annotations::for_flavor(
                flavor,
                // Stamped here: a pipeline without `--identifier` learns the tag only after the index is written.
                identifier.tag().and_then(Version::parse).as_ref(),
            ),
        };
        let annotations = merge_annotations(generated, &self.annotation);

        let outcome = if self.cascade {
            let existing_tags = publisher
                .list_tags(target.clone())
                .await
                .with_context(|| format!("listing existing tags for {identifier}"))?;

            let existing_versions = Publisher::parse_versions(&existing_tags);
            publisher
                .push_cascade(
                    &target,
                    infos,
                    &self.layers,
                    existing_versions,
                    build_meta.as_deref(),
                    keep_tag,
                    self.default,
                    &annotations,
                )
                .await?
        } else {
            publisher
                .push(
                    &target,
                    infos,
                    &self.layers,
                    build_meta.as_deref(),
                    keep_tag,
                    self.default,
                    &annotations,
                )
                .await?
        };

        // No `__ocx.keep.*` tags: announce drops them, so the file would name tags never announced.
        let mut pushed_tags = vec![outcome.primary_tag.clone()];
        pushed_tags.extend(outcome.cascade_tags.iter().cloned());
        pushed_tags.extend(outcome.aliases_written.iter().cloned());

        // Platform manifests, never the index, whose digest a later platform merge rewrites.
        let platform_digests = outcome.platform_digests.clone();
        // `--build-timestamp` rewrites the tag at publish time, so the report names the tag written, not the input.
        let pushed = identifier.clone_with_tag(&outcome.primary_tag).to_string();
        let mut report =
            crate::api::data::push::PushReport::from_outcome(pushed, outcome).with_annotations(annotations);
        // Post-push failures are never rolled back: each becomes a report row plus a log line.
        let mut failures: Vec<ocx_exit::ExitCode> = Vec::new();

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
                        // A `both` platform that lost one leg fails but still reports the leg that landed.
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
                        failures.push(crate::exit::classify_library_error(error.as_ref()));
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

        // Folded into the report, never raised: the push already landed and its report is owed.
        if let Some(predicate) = sbom_predicate {
            let options = signing.expect("--sbom resolves the signing options above");
            match Self::attest_sbom(&context, &identifier, &platform, options, predicate).await {
                Ok(outcome) => report = report.with_attestation(outcome),
                Err(err) => {
                    report = report.with_attestation(package_sign_common::failed_outcome(&err));
                    failures.push(crate::exit::classify_library_error(err.as_ref()));
                    log::error!("{}", crate::api::data::sanitize_for_terminal(&format!("{err:#}")));
                }
            }
        }
        context.api().report(&report)?;

        // After the report, so a tags-file failure never swallows a push that already landed.
        if let Some(path) = &self.tags_file
            && let Err(error) = conventions::append_tags_file(path, &pushed_tags).await
        {
            context.ui().warn(format!(
                "the push succeeded but the tags file {} was not written",
                path.display()
            ));
            return Err(error);
        }

        // Returned, not raised: stdout already holds the report, so an error envelope would be suppressed.
        Ok(ExitCode::from(package_sign_common::sweep_exit_code(&failures)))
    }

    /// Resolves the one option set both `--sign` and `--sbom` sign under.
    ///
    /// # Errors
    ///
    /// A forbidden endpoint URL, a malformed `--key` (exit 64), an unimplemented key backend
    /// (exit 85) or a keyless `--no-rekor-upload` (exit 64), each a `SignError` carrying the identifier.
    async fn resolve_signing(
        &self,
        context: &crate::app::Context,
        identifier: &ocx_oci::PackageRef,
    ) -> anyhow::Result<ocx_package_manager::SignOptions> {
        let (fulcio_url, rekor_url) = package_sign_common::resolve_sigstore_pair(
            context.config_trust_sigstore(),
            identifier,
            self.fulcio_url.as_deref(),
            self.rekor_url.as_deref(),
        )?;
        // Wrapped in `SignError`, or `classify_error` misses the bare kind and exits 1 with no identifier.
        let key = self.key.reference().map_err(|kind| {
            ocx_sign::sign::SignError::new(identifier.clone(), ocx_sign::sign::SignErrorKind::from(kind))
        })?;
        let configured_rekor_upload = context
            .config_trust_sigstore()
            .and_then(|sigstore| sigstore.rekor_upload);
        let rekor_upload = self
            .rekor_upload
            .enabled(self.key.is_key_mode(), configured_rekor_upload)
            .map_err(|kind| ocx_sign::sign::SignError::new(identifier.clone(), kind))?;
        let identity_token = package_sign_common::resolve_override_token(None, false, identifier).await?;
        Ok(ocx_package_manager::SignOptions {
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

    /// Attests `predicate` as a CycloneDX SBOM against the manifest this push wrote for `platform`,
    /// resolved through `identifier`, never a keep tag (`--no-keep-tag` may have suppressed it).
    ///
    /// # Errors
    ///
    /// Any attest-pipeline failure, re-rooted so the JSON envelope keeps `context.identifier`.
    async fn attest_sbom(
        context: &crate::app::Context,
        identifier: &ocx_oci::PackageRef,
        platform: &ocx_oci::Platform,
        options: ocx_package_manager::SignOptions,
        predicate: Vec<u8>,
    ) -> anyhow::Result<crate::api::data::push::AttestationOutcome> {
        use ocx_sign::attest::predicate::PredicateType;

        let result = context
            .manager()
            .attest_one(
                identifier,
                Some(platform),
                ocx_package_manager::AttestOptions {
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
        // Neither is required: `simplesigning` writes only the `.att` sidecar, still a published attestation.
        Ok(crate::api::data::push::AttestationOutcome::Succeeded {
            referrer_digest: result.referrer.map(|leg| leg.manifest_digest.to_string()),
            sidecar_digest: result.sidecar.map(|leg| leg.manifest_digest.to_string()),
            predicate_type: result.predicate_type,
            signed: result.signed,
        })
    }
}

/// Splits a `KEY=VALUE` annotation argument at the first `=`; only the key must be non-empty.
pub(crate) fn parse_annotation(argument: &str) -> anyhow::Result<(String, String)> {
    let (key, value) = argument
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected KEY=VALUE, got '{argument}'"))?;
    if key.is_empty() {
        return Err(anyhow::anyhow!("annotation key is empty in '{argument}'"));
    }
    Ok((key.to_string(), value.to_string()))
}

/// Lays the explicit `--annotation` pairs over the generated `--ci-annotations` set, last value
/// winning; reversed, the environment would overwrite what a user typed.
fn merge_annotations(generated: BTreeMap<String, String>, explicit: &[(String, String)]) -> BTreeMap<String, String> {
    let mut merged = generated;
    merged.extend(explicit.iter().cloned());
    merged
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

    use super::{merge_annotations, parse_annotation};

    fn pair(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

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

    /// The precedence `--ci-annotations` promises: what the publisher typed
    /// beats what the environment happened to hold. A wrapper that already
    /// knows the right source URL must be able to state it and be obeyed.
    #[test]
    fn an_explicit_annotation_overrides_the_generated_one() {
        let generated = BTreeMap::from([
            pair("org.opencontainers.image.source", "https://gitlab.example/ci/job"),
            pair("org.opencontainers.image.revision", "cafebabe"),
        ]);

        let merged = merge_annotations(
            generated,
            &[pair(
                "org.opencontainers.image.source",
                "https://github.com/acme/widget",
            )],
        );

        assert_eq!(
            merged.get("org.opencontainers.image.source").map(String::as_str),
            Some("https://github.com/acme/widget"),
            "the explicit --annotation lost to the generated value: {merged:?}"
        );
        assert_eq!(
            merged.get("org.opencontainers.image.revision").map(String::as_str),
            Some("cafebabe"),
            "a generated key nothing overrode must survive: {merged:?}"
        );
    }

    /// Last-wins among the explicit pairs themselves is unchanged by the
    /// merge, generated set or not.
    #[test]
    fn repeated_explicit_keys_still_keep_the_last_value() {
        let merged = merge_annotations(BTreeMap::new(), &[pair("a", "first"), pair("a", "second")]);

        assert_eq!(merged.get("a").map(String::as_str), Some("second"));
    }
}

#[cfg(test)]
mod keep_tag_help_tests {
    use clap::CommandFactory as _;

    #[test]
    fn the_keep_tag_long_help_says_when_to_disable_it() {
        let command = super::PackagePush::command();
        let arg = command
            .get_arguments()
            .find(|a| a.get_id() == "keep_tag")
            .expect("keep_tag");
        let help = arg.get_long_help().expect("long help").to_string();
        assert!(help.contains("push ephemeral builds with `--no-keep-tag`, or prune frees no storage"));
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
    use ocx_sign::sign::SignatureFormat;

    use super::PackagePush;

    /// Every modifier flag, spelled as argv.
    ///
    /// The two endpoint flags join this list rather
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
            matches!(error, ocx_sign::sign::SignErrorKind::RekorUploadRequiredForKeyless),
            "got: {error}"
        );
    }

    // ── the two endpoint flags ──────────────────────────────────────────

    /// `--fulcio-url` is keyless-only and conflicts with `--key`;
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

    /// The endpoint pair `push` resolves prefers its own flags
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
    /// the whole reason ocx-mirror needs them.
    #[test]
    fn the_resolved_endpoint_pair_prefers_pushs_flags_over_trust_sigstore() {
        let configured = ocx_trust::SigstoreTrust {
            fulcio_url: Some("https://fleet-fulcio.example".to_string()),
            rekor_url: Some("https://fleet-rekor.example".to_string()),
            ..ocx_trust::SigstoreTrust::default()
        };
        let identifier = ocx_oci::PackageRef::parse("registry.example/pkg:1.0").expect("static parse");

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

#[cfg(test)]
mod ci_annotations_flag_tests {
    //! The space form of `--ci-annotations`, which the grammar cannot take as
    //! a value.
    //!
    //! `require_equals` is a shipped contract on `--ci` too, so it stays: the
    //! fix is a refusal, not a looser grammar. Every test here parses real
    //! argv, because the absorption under test is the parser's doing — a test
    //! that hand-built `ci_annotations` and `layers` would assert only that the
    //! guard reads its own arguments.

    use clap::Parser as _;

    use super::PackagePush;

    fn parse(extra: &[&str]) -> PackagePush {
        let mut argv = vec!["push"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["--identifier", "registry.example/pkg:1.0"]);
        PackagePush::try_parse_from(argv).expect("every argv here is grammatical")
    }

    /// The code the process would exit with, through the same authority
    /// `main.rs` uses.
    fn exit_code(error: &anyhow::Error) -> u8 {
        crate::exit::classify_error(error.as_ref()) as u8
    }

    /// The premise the guard rests on: clap leaves the flag bare and hands the
    /// value to the layer positional. If a clap upgrade ever attached it, the
    /// guard would become dead code — and this is what would say so.
    #[test]
    fn a_spaced_value_never_reaches_the_flag() {
        let push = parse(&["--ci-annotations", "gitlab"]);

        assert_eq!(push.ci_annotations, Some(None), "the flag must be left bare");
        assert_eq!(
            push.layers.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["gitlab"],
            "the value must have fallen through to the layer positional"
        );
    }

    /// Exit 64, naming the token and the `=` requirement. Every spelling the
    /// `=` form would have accepted, aliases included: a guard that caught only
    /// the canonical two would pass `--ci-annotations gitlab-ci` silently.
    #[test]
    fn a_spaced_value_is_refused_with_exit_64() {
        for value in ["github", "gitlab", "github-actions", "gitlab-ci", "GitLab", "GITHUB"] {
            let error = anyhow::Error::new(
                parse(&["--ci-annotations", value])
                    .refuse_spaced_ci_annotations()
                    .expect_err("a spaced value must be refused"),
            );

            assert_eq!(exit_code(&error), 64, "a usage fault is exit 64: {error:#}");
            let message = error.to_string();
            // `contains(value)` alone would pass on the advice clause below
            // ("pass --ci-annotations=github or ...") without the message ever
            // having echoed the token. Anchor it to the phrase that quotes it.
            assert!(
                message.contains("--ci-annotations") && message.contains(&format!("was given `{value}`")),
                "the refusal must name the flag and echo the token: {message}"
            );
            assert!(
                message.contains("--ci-annotations=gitlab") && message.contains("attached with `=`"),
                "the refusal must name the `=` requirement: {message}"
            );
        }
    }

    /// The negative controls, every shape that must still pass: a bare flag
    /// with ordinary layers (autodetect is legitimate), the attached form, and
    /// no flag at all. The `--ci-annotations=gitlab gitlab` row is the one that
    /// pins the bare-flag gate: with the value attached, a layer named `gitlab`
    /// is what the publisher meant, so the heuristic must not reach it.
    ///
    /// The bare `./gitlab` row is the escape the guard's own comment promises an
    /// operator who really has a layer by that name — the promise is worth
    /// nothing if the only thing tested is a longer path that happens to
    /// contain the word.
    #[test]
    fn ordinary_invocations_are_not_refused() {
        for argv in [
            vec!["--ci-annotations", "./layer.tar.gz"],
            vec!["--ci-annotations", "./gitlab"],
            vec!["--ci-annotations", "./GitLab"],
            vec!["--ci-annotations", "./gitlab/layer.tar.gz"],
            vec!["--ci-annotations=gitlab", "./layer.tar.gz"],
            vec!["--ci-annotations=gitlab", "gitlab"],
            vec!["--ci-annotations=gitlab"],
            vec!["./layer.tar.gz"],
            vec![],
        ] {
            parse(&argv)
                .refuse_spaced_ci_annotations()
                .unwrap_or_else(|error| panic!("{argv:?} must not be refused: {error}"));
        }
    }
}

#[cfg(test)]
mod build_timestamp_flag_tests {
    //! The space form of `--build-timestamp`, the sibling of the
    //! `--ci-annotations` module above on the same command and the same
    //! `layers` positional — and the worse of the two.
    //!
    //! `--ci-annotations` loses an annotation. This flag carries
    //! `default_missing_value = "datetime"`, so the absorbed value is replaced
    //! by its **opposite**: `--build-timestamp none` publishes a timestamped
    //! version. Every test parses real argv, because the absorption under test
    //! is the parser's doing.

    use clap::Parser as _;
    use ocx_package::version::BuildTimestampFormat;

    use super::PackagePush;

    fn parse(extra: &[&str]) -> PackagePush {
        let mut argv = vec!["push"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["--identifier", "registry.example/pkg:1.0"]);
        PackagePush::try_parse_from(argv).expect("every argv here is grammatical")
    }

    fn exit_code(error: &anyhow::Error) -> u8 {
        crate::exit::classify_error(error.as_ref()) as u8
    }

    /// The premise, and the whole reason this flag outranks `--ci-annotations`:
    /// the value does not merely fail to attach, it is replaced by the
    /// `default_missing_value` — so `--build-timestamp none` asks for no build
    /// metadata and resolves to `Datetime`.
    ///
    /// This also pins the guard's precondition. `refuse_spaced_build_timestamp`
    /// tests for `Some(Datetime)` specifically, which is only sound while
    /// `Datetime` is what a bare flag resolves to; repointing
    /// `default_missing_value` without this test would leave the guard
    /// silently narrower than the grammar.
    #[test]
    fn a_bare_build_timestamp_still_resolves_to_datetime() {
        for (argv, why) in [
            (vec!["--build-timestamp"], "the bare flag takes the default"),
            (
                vec!["--build-timestamp", "none"],
                "the spaced value is replaced by the default - the inversion",
            ),
        ] {
            let push = parse(&argv);
            assert_eq!(
                push.build_timestamp,
                Some(BuildTimestampFormat::Datetime),
                "{why}: {argv:?}"
            );
        }

        // ...and the value really did land on the layer positional, which is
        // what makes the wrong-artifact case reachable rather than theoretical.
        assert_eq!(
            parse(&["--build-timestamp", "none"])
                .layers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["none"],
            "the value must have fallen through to the layer positional"
        );
    }

    /// Exit 64 for all three spellings, plus the mis-cased forms the `=` form
    /// would itself reject. `none` is the row that matters most: it is the one
    /// whose silent absorption publishes the opposite of what was asked.
    #[test]
    fn a_spaced_value_is_refused_with_exit_64() {
        for value in ["datetime", "date", "none", "None", "DATE", "DateTime"] {
            let error = anyhow::Error::new(
                parse(&["--build-timestamp", value])
                    .refuse_spaced_build_timestamp()
                    .expect_err("a spaced value must be refused"),
            );

            assert_eq!(exit_code(&error), 64, "a usage fault is exit 64: {error:#}");
            let message = error.to_string();
            // Anchored to the phrase that quotes the token: `contains(value)`
            // alone would pass on the advice clause, which lists every
            // spelling without the message having echoed what was typed.
            assert!(
                message.contains("--build-timestamp") && message.contains(&format!("was given `{value}`")),
                "the refusal must name the flag and echo the token: {message}"
            );
            assert!(
                message.contains("--build-timestamp=none") && message.contains("attached with `=`"),
                "the refusal must list the value vocabulary and the `=` requirement: {message}"
            );
        }
    }

    /// The negative controls. Two rows carry the weight:
    ///
    /// - `--build-timestamp=date none` proves the guard reads the *variant*,
    ///   not merely "the flag is present": `Date` cannot have come from the
    ///   default, so a layer named `none` beside it is what the publisher
    ///   meant. Without that narrowing this row would be refused.
    /// - `--build-timestamp ./none` is the escape the guard's comment promises
    ///   an operator who really has such a layer, and `LayerRef`'s own doc
    ///   already documents `./` as the disambiguator.
    #[test]
    fn ordinary_invocations_are_not_refused() {
        for argv in [
            vec!["--build-timestamp", "./layer.tar.gz"],
            vec!["--build-timestamp", "./none"],
            vec!["--build-timestamp", "./date"],
            vec!["--build-timestamp", "none.tar.gz"],
            vec!["--build-timestamp=date", "none"],
            vec!["--build-timestamp=none", "datetime"],
            vec!["--build-timestamp=none", "./layer.tar.gz"],
            vec!["--build-timestamp=datetime"],
            vec!["none"],
            vec!["datetime", "./layer.tar.gz"],
            vec![],
        ] {
            parse(&argv)
                .refuse_spaced_build_timestamp()
                .unwrap_or_else(|error| panic!("{argv:?} must not be refused: {error}"));
        }
    }
}
