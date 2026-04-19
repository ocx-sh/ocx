// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    log, oci, package,
    prelude::*,
    publisher::{LayerRef, Publisher},
};

use crate::options;

#[derive(Parser)]
pub struct PackagePush {
    /// Will cascade rolling releases, ie. pushing 1.2.3 will also update 1.2, 1, etc.
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    /// Indicates that this is a new package that doesn't exist in the registry yet.
    /// This will skip some checks that requires an existing index.
    #[clap(long = "new", short = 'n')]
    new: bool,

    /// Path to the package metadata JSON file. Defaults to a sibling of the
    /// first file layer (e.g. `pkg.tar.gz` → `pkg-metadata.json`). Required
    /// when no file layers are provided.
    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,

    /// Target platform (e.g. `linux/amd64`). Required.
    #[clap(short, long, required = true)]
    platform: oci::Platform,

    identifier: options::Identifier,

    /// Layers to push, in order (base layer first, top layer last).
    ///
    /// Each layer is either:
    ///   - a path to a pre-built archive file (`.tar.gz`, `.tar.xz`), or
    ///   - a digest reference to a layer already present in the target
    ///     registry, written as `sha256:<hex>.<ext>` where `<ext>` declares
    ///     the original archive format — one of `tar.gz`, `tgz`, `tar.xz`,
    ///     `txz`. The OCI distribution spec does not expose a layer's media
    ///     type via blob HEAD, so the suffix is required: OCX refuses to
    ///     guess.
    ///
    /// Digest references enable layer reuse: a base layer pushed once can be
    /// referenced by digest from many packages without re-uploading. Zero
    /// layers is valid (produces a config-only OCI artifact) when
    /// `--metadata` is supplied.
    ///
    /// Examples:
    ///   ocx package push repo:2.0.0 sha256:<hex>.tar.gz ./new.tar.gz
    ///   ocx package push repo:2.0.0 sha256:<hex>.tar.xz
    layers: Vec<LayerRef>,
}

impl PackagePush {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // Infer metadata from the first file layer, or require --metadata for digest-only pushes.
        let metadata_path = match &self.metadata {
            Some(path) => path.clone(),
            None => {
                let first_file = self.layers.iter().find_map(|l| match l {
                    LayerRef::File(p) => Some(p.as_path()),
                    LayerRef::Digest { .. } => None,
                });
                let file_path = first_file
                    .ok_or_else(|| anyhow::anyhow!("--metadata is required when no file layers are provided"))?;
                crate::conventions::infer_metadata_file(file_path)?
            }
        };

        log::info!(
            "Deploying package with {} layer(s) and metadata {}",
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

        if self.cascade {
            let existing_tags = match publisher.list_tags(identifier.clone()).await {
                Ok(tags) => tags,
                Err(err) => {
                    if self.new {
                        log::info!("Failed to list tags, assuming new package: {err}");
                        Vec::new()
                    } else {
                        return Err(anyhow::anyhow!(
                            "Failed to list existing tags for {}: {err}",
                            identifier
                        ));
                    }
                }
            };

            let existing_versions = Publisher::parse_versions(&existing_tags);
            publisher.push_cascade(info, &self.layers, existing_versions).await?;
        } else {
            publisher.push(info, &self.layers).await?;
        }

        Ok(ExitCode::SUCCESS)
    }
}
