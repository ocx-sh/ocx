// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    log, oci, package,
    package::version::{BuildTimestampFormat, build_timestamp},
    prelude::*,
    publisher::{LayerRef, Publisher},
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

    /// Target platform (e.g. `linux/amd64`). Required.
    #[clap(short, long, required = true)]
    platform: oci::Platform,

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
        let metadata =
            package::metadata::ValidMetadata::try_from(package::metadata::Metadata::read_json(&metadata_path).await?)?;
        let info = package::info::Info {
            identifier: identifier.clone(),
            metadata: metadata.into(),
            platform: self.platform.clone(),
        };

        let publisher = Publisher::new(context.remote_client()?.clone());
        publisher.ensure_auth(&identifier).await?;

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
                .push_cascade(info, &self.layers, existing_versions, build_meta.as_deref())
                .await?
        } else {
            publisher.push(info, &self.layers, build_meta.as_deref()).await?
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
}
