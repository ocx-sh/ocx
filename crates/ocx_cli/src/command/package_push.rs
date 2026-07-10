// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    log, oci, package,
    package::version::{BuildTimestampFormat, build_timestamp},
    prelude::*,
    publisher::{self, LayerRef, Publisher},
};

use crate::{conventions, options};

#[derive(Parser)]
pub struct PackagePush {
    /// Will cascade rolling releases, ie. pushing 1.2.3 will also update 1.2, 1, etc.
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    /// Indicates that this is a new package that doesn't exist in the registry yet.
    /// This will skip some checks that requires an existing index.
    #[clap(long = "new", short = 'n')]
    new: bool,

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
    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,

    /// Target platform (e.g. `linux/amd64`)
    ///
    /// When the metadata sidecar carries a target-platform set (written by
    /// `ocx package create --platform`), omitting this flag pushes EVERY
    /// platform in the set; passing it narrows the push to that platform,
    /// which must be a member of the set. Without an embedded set the flag
    /// is required and selects the single platform to publish.
    #[clap(short, long)]
    platform: Option<oci::Platform>,

    /// Identifier under which the package is published (e.g. `repo:2.0.0`).
    #[clap(short = 'i', long = "identifier", required = true)]
    identifier: options::Identifier,

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
    /// Either form may carry an optional layout tail `:strip=N,prefix=P` that
    /// controls how the layer is placed when the package is installed:
    ///   - `strip=N` drops the leading N path components (like
    ///     `tar --strip-components=N`).
    ///   - `prefix=P` relocates the layer under the relative subdirectory `P`
    ///     (must stay inside the package; `..`, absolute, and Windows-style
    ///     paths are rejected).
    ///
    /// Both keys are optional and comma-separated; omit the tail for the
    /// default (no strip, package root).
    ///
    /// Digest references enable layer reuse: a base layer pushed once can be
    /// referenced by digest from many packages without re-uploading. Zero
    /// layers is valid (produces a config-only OCI artifact) when
    /// `--metadata` is supplied.
    ///
    /// Examples:
    ///   ocx package push repo:2.0.0 ./libs.tar.gz:strip=1,prefix=share
    ///   ocx package push repo:2.0.0 sha256:<hex>.tar.xz ./new.tar.zst
    layers: Vec<LayerRef>,
}

impl PackagePush {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        let metadata_path = conventions::resolve_metadata_path(&self.layers, self.metadata.as_deref())?;

        log::info!(
            "deploying package with {} layer(s) and metadata {}",
            self.layers.len(),
            metadata_path.display()
        );
        let metadata = package::metadata::authoring::AuthoringMetadata::read_json(&metadata_path).await?;
        let platforms = self.select_platforms(&metadata)?;

        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(&identifier).await?;

        // Gate: every dependency must project to an existing platform
        // MANIFEST digest for every fan-out platform — push makes no
        // resolution decisions (run `ocx package create` for that).
        {
            let _spin = context.progress().spinner("Verifying dependency pins");
            publisher::verify_dependency_pins(publisher.client(), &metadata, &platforms).await?;
        }

        // One Info per target platform, each carrying the published
        // projection of the metadata for that platform.
        let infos = platforms
            .iter()
            .map(|platform| {
                let published = metadata.to_published(platform)?;
                let valid = package::metadata::ValidMetadata::try_from(published)?;
                Ok(package::info::Info {
                    identifier: identifier.clone(),
                    metadata: valid.into(),
                    platform: platform.clone(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let build_meta: Option<String> = self.build_timestamp.as_ref().and_then(build_timestamp);

        let outcome = if self.cascade {
            let existing_tags = match publisher.list_tags(identifier.clone()).await {
                Ok(tags) => tags,
                Err(err) => {
                    if self.new {
                        log::info!("failed to list tags, assuming new package: {err}");
                        Vec::new()
                    } else {
                        return Err(anyhow::anyhow!(
                            "failed to list existing tags for {}: {err}",
                            identifier
                        ));
                    }
                }
            };

            let existing_versions = Publisher::parse_versions(&existing_tags);
            publisher
                .push_cascade(infos, &self.layers, existing_versions, build_meta.as_deref())
                .await?
        } else {
            publisher.push(infos, &self.layers, build_meta.as_deref()).await?
        };

        // Emit the structured push report. Plain output is a one-row table
        // (identifier, digest, cascade tags); `--format json` serializes the
        // report consumed by `ocx-mirror pipeline push`.
        context.api().report(&crate::api::data::push::PushReport::new(
            identifier.to_string(),
            outcome.manifest_digest.to_string(),
            outcome.cascade_tags,
        ))?;

        Ok(ExitCode::SUCCESS)
    }

    /// Decide the fan-out platform set (decision D1 of
    /// `adr_dependency_manifest_pinning.md`): an embedded target set wins;
    /// `--platform` is a member-filter/assert against it. A legacy sidecar
    /// without an embedded set keeps requiring `--platform`.
    fn select_platforms(
        &self,
        metadata: &ocx_lib::package::metadata::authoring::AuthoringMetadata,
    ) -> anyhow::Result<Vec<oci::Platform>> {
        use ocx_lib::cli::UsageError;

        match metadata.target_platforms() {
            Some(set) => match &self.platform {
                None => Ok(set.iter().cloned().collect()),
                Some(platform) if set.contains(platform) => Ok(vec![platform.clone()]),
                Some(platform) => {
                    let members = set.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
                    Err(UsageError::new(format!(
                        "--platform '{platform}' is not in the package target set [{members}]; \
                         drop --platform to push the whole set or re-run ocx package create for '{platform}'"
                    ))
                    .into())
                }
            },
            None => match &self.platform {
                Some(platform) => Ok(vec![platform.clone()]),
                None => Err(UsageError::new(
                    "the metadata sidecar carries no target-platform set; pass --platform (-p) \
                     or re-run ocx package create --platform to embed one",
                )
                .into()),
            },
        }
    }
}
