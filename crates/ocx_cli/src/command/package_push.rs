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

use crate::command::package_sign_common;
use crate::{conventions, options};

#[derive(Parser)]
pub struct PackagePush {
    /// Will cascade rolling releases, ie. pushing 1.2.3 will also update 1.2, 1, etc.
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    /// Push a `sha256.<hex>` tag pointing at each pushed platform manifest
    /// (default). A stray delete of a rolling or cascade tag can then never
    /// orphan a digest something else still pins, since the canonical tag
    /// names it directly. Pass `--no-canonical-tag` to skip it.
    #[clap(flatten)]
    canonical_tag: options::CanonicalTag,

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
    /// --tags-from-file` can pick them up.
    ///
    /// This is a scratch file for one pipeline run, not a persistent list -
    /// a stale file left over from an earlier run could re-add a tag that
    /// was deliberately dropped from a later announce.
    #[clap(long = "announce-file", value_name = "PATH")]
    announce_file: Option<std::path::PathBuf>,

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
        let canonical_tag = self.canonical_tag.enabled();
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
                    canonical_tag,
                    &annotations,
                )
                .await?
        } else {
            publisher
                .push(infos, &self.layers, build_meta.as_deref(), canonical_tag, &annotations)
                .await?
        };

        // The primary version tag plus the rolling cascade tags. Canonical
        // `sha256.<hex>` tags are deliberately left out: announce drops them
        // downstream, so recording one in a file named "announce" would state
        // something that never gets announced.
        let mut pushed_tags = vec![identifier.tag_or_latest().to_string()];
        pushed_tags.extend(outcome.cascade_tags.iter().cloned());

        // Emit the structured push report BEFORE the announce-file append. The
        // push itself already succeeded and is not undoable, so an I/O failure
        // writing the scratch file must not swallow the report — the caller
        // still has to learn what landed in the registry. Plain output is a
        // one-row table (identifier, digest, cascade + canonical tags, layer
        // counts); `--format json`
        // serializes the report consumed by `ocx-mirror pipeline push`.
        let mut report = crate::api::data::push::PushReport::from_outcome(identifier.to_string(), outcome);

        // The push already landed. Whatever the attestation does, the report is
        // owed to the caller — so the outcome is folded into the report rather
        // than replacing it with an error envelope, and the error is returned
        // only after the report is on stdout.
        let mut attest_failure = None;
        if let Some(predicate) = sbom_predicate {
            match self.attest_sbom(&context, &identifier, &platform, predicate).await {
                Ok(outcome) => report = report.with_attestation(outcome),
                Err(err) => {
                    report = report.with_attestation(package_sign_common::failed_outcome(&err));
                    attest_failure = Some(err);
                }
            }
        }
        context.api().report(&report)?;

        // The append still decides the exit code: the caller asked for the file,
        // so a failure is a failure — it just no longer costs them the report.
        if let Some(path) = &self.announce_file
            && let Err(error) = append_to_announce_file(path, &pushed_tags).await
        {
            context.ui().warn(format!(
                "the push succeeded but the announce file {} was not written",
                path.display()
            ));
            return Err(error);
        }

        // The push succeeded, so the attestation failure is the worst outcome
        // in the run and owns the exit code. `main` classifies it through the
        // same chain `ocx package attest` uses, and the envelope is suppressed
        // because the push report already claimed stdout.
        if let Some(error) = attest_failure {
            return Err(error);
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Attest `predicate` as a CycloneDX SBOM against the manifest this push
    /// wrote for `platform`.
    ///
    /// The subject digest is resolved by the attest pipeline from the
    /// identifier and platform, never derived from a canonical tag —
    /// `--no-canonical-tag` may have suppressed those.
    ///
    /// # Errors
    ///
    /// Any attest-pipeline failure, already re-rooted so the JSON envelope
    /// keeps its `context.identifier`.
    async fn attest_sbom(
        &self,
        context: &crate::app::Context,
        identifier: &oci::Identifier,
        platform: &oci::Platform,
        predicate: Vec<u8>,
    ) -> anyhow::Result<crate::api::data::push::AttestationOutcome> {
        use ocx_lib::oci::attest::predicate::PredicateType;

        // `push` exposes no endpoint flags, so `None` for both puts it on the
        // tail of the same ladder `attest` walks: `[trust.sigstore]` > builtin
        // default, then the same SSRF guard and the same refusal kind.
        let (fulcio_url, rekor_url) =
            package_sign_common::resolve_sigstore_pair(context.config_trust_sigstore(), identifier, None, None)?;
        let identity_token = package_sign_common::resolve_override_token(None, false, identifier).await?;
        let result = context
            .manager()
            .attest_one(
                identifier,
                platform,
                ocx_lib::package_manager::AttestOptions {
                    fulcio_url,
                    rekor_url,
                    identity_token,
                    predicate_type: PredicateType::CycloneDx,
                    predicate,
                    no_cache: false,
                    no_tty: false,
                    offline: context.is_offline(),
                },
            )
            .await
            .map_err(package_sign_common::attest_error_into_anyhow)?
            .result;
        Ok(crate::api::data::push::AttestationOutcome::Succeeded {
            referrer_digest: result.referrer_digest.to_string(),
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

/// Appends `tags` onto the announce-file at `path` (created if absent),
/// deduping against whatever is already there (design register C2).
async fn append_to_announce_file(path: &std::path::Path, tags: &[String]) -> anyhow::Result<()> {
    let existing = match tokio::fs::read(path).await {
        Ok(bytes) => conventions::parse_tags_file(&bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err).with_context(|| format!("reading announce file {}", path.display())),
    };
    let merged = conventions::merge_tags_file(&existing, tags);
    tokio::fs::write(path, merged)
        .await
        .with_context(|| format!("writing announce file {}", path.display()))
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
mod announce_file_tests {
    use super::append_to_announce_file;

    #[tokio::test]
    async fn creates_the_file_with_the_pushed_and_cascade_tags() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("announce.txt");

        append_to_announce_file(
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

        append_to_announce_file(&path, &["3.28.1".to_string(), "3.28".to_string()])
            .await
            .expect("first append succeeds");
        append_to_announce_file(&path, &["3.28.2".to_string(), "3.28".to_string()])
            .await
            .expect("second append succeeds");

        let content = tokio::fs::read_to_string(&path).await.expect("read announce file");
        assert_eq!(content, "3.28.1,3.28,3.28.2");
    }
}
