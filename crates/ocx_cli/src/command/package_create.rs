// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{compression, log, oci, package, package::metadata::authoring::AuthoringMetadata, prelude::*};

use crate::options;

#[derive(Parser)]
pub struct PackageCreate {
    /// Path to the package to bundle
    path: std::path::PathBuf,
    /// Optional identifier for the package, used to infer the output filename if not specified
    #[clap(short, long)]
    identifier: Option<options::Identifier>,
    /// Platform of the package content (e.g. `linux/amd64`, or `any` for platform-agnostic content)
    ///
    /// When metadata declares dependencies without a digest, this flag is
    /// required: create resolves each one against the selected index to a
    /// platform manifest digest and rewrites the metadata sidecar with the
    /// resolved pins. Resolution honors `--remote`, `--offline`, and
    /// `--frozen`. When `--metadata` is given, this platform is also
    /// recorded in the sidecar; `ocx package push` and `ocx package test`
    /// default to it and reject a `--platform` that disagrees. Also used
    /// to infer the output filename.
    #[clap(short, long)]
    platform: Option<oci::Platform>,
    /// Output file or directory, if a directory is provided the filename will be inferred
    #[clap(short, long)]
    output: Option<std::path::PathBuf>,
    /// Force overwrite of output file if it already exists
    #[clap(short, long)]
    force: bool,
    /// Path to a `metadata.json` file to validate, resolve, and write alongside the output bundle
    ///
    /// Dependencies without a digest are pinned to platform manifest digests
    /// (requires `--platform`); the resolved sidecar is written next to the
    /// output bundle in canonical form.
    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,
    /// Compression level to use for the package bundle
    #[arg(short = 'l', long, value_enum, default_value_t = options::CompressionLevel::Default)]
    compression_level: options::CompressionLevel,
    /// Number of compression threads (0 = auto-detect, 1 = single-threaded)
    #[arg(short = 'j', long, default_value_t = 0)]
    threads: u32,
}

impl PackageCreate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = options::Identifier::transform_optional(self.identifier.clone(), context.default_registry())?;
        let output = match &self.output {
            Some(output) => {
                let is_dir = tokio::fs::metadata(output).await.map(|m| m.is_dir()).unwrap_or(false);
                if is_dir {
                    output.join(self.infer_filename(identifier.as_ref()))
                } else {
                    output.clone()
                }
            }
            None => self.infer_filename(identifier.as_ref()).into(),
        };

        if tokio::fs::try_exists(&output).await? && !self.force {
            anyhow::bail!(
                "output file {} already exists; use --force to overwrite",
                output.display()
            );
        }

        // Resolve + validate the sidecar BEFORE writing the output bundle:
        // dependency resolution can fail (network / policy / missing tag /
        // empty platform intersection), and a failure must leave no orphan
        // bundle on disk (Codex #3). Only after the metadata is fully validated
        // do we build the archive and, last, write the resolved sidecar.
        let resolved_metadata = match &self.metadata {
            Some(metadata_source) => {
                let metadata = AuthoringMetadata::read_json(metadata_source.as_path()).await?;
                let metadata = self.resolve_dependency_pins(metadata, &context).await?;
                let platform = self.validation_platform();
                // Validate the projection the publisher will actually push:
                // run the publish-time env/entrypoint checks against the
                // declared platform.
                package::metadata::ValidMetadata::try_from(metadata.to_published(&platform)?)?;
                // Record the platform dependency pins were resolved against
                // so `ocx package push`/`ocx package test` can bind to it
                // instead of silently defaulting to the host platform.
                Some(metadata.with_platform(platform))
            }
            None => None,
        };

        if let Some(parent) = output.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let compression_options =
            compression::CompressionOptions::from_level(self.compression_level.into()).with_threads(self.threads);
        log::info!(
            "Creating package bundle from {} with compression level {:?}",
            self.path.display(),
            self.compression_level
        );
        {
            let _spin = context.progress().spinner(format!("Bundling {}", self.path.display()));
            package::bundle::BundleBuilder::from_path(&self.path)
                .with_compression(compression_options)
                .create(&output)
                .await?;
        }
        log::info!(
            "Created package bundle from {} at {}",
            self.path.display(),
            output.display()
        );

        if let Some(metadata) = resolved_metadata {
            // Always rewrite the sidecar canonically (never a byte copy): the
            // file next to the bundle is the compiled, pin-resolved form.
            let metadata_target = crate::conventions::infer_metadata_file(&output)?;
            metadata.write_json(&metadata_target).await?;
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Pick the platform to run publish-time validation against: the declared
    /// `--platform`, or the host platform (falling back to `any`) when a
    /// pre-pinned sidecar was supplied without one.
    fn validation_platform(&self) -> oci::Platform {
        self.platform
            .clone()
            .unwrap_or_else(|| oci::Platform::current().unwrap_or_else(oci::Platform::any))
    }

    /// Resolve unpinned dependencies against the selected index.
    ///
    /// - `--platform` given: resolve every unpinned dependency against it
    ///   (already-pinned dependencies pass through untouched, no network).
    /// - `--platform` omitted: metadata must already be fully pinned (usage
    ///   error otherwise); passes through for canonical rewrite only.
    async fn resolve_dependency_pins(
        &self,
        metadata: AuthoringMetadata,
        context: &crate::app::Context,
    ) -> anyhow::Result<AuthoringMetadata> {
        let Some(platform) = &self.platform else {
            if !metadata.is_fully_pinned() {
                return Err(ocx_lib::cli::UsageError::new(
                    "metadata declares dependencies that are not pinned to a manifest digest; \
                     pass --platform (-p) so ocx package create can resolve them",
                )
                .into());
            }
            return Ok(metadata);
        };
        let _spin = context.progress().spinner("Resolving dependency pins");
        Ok(package::dependency_pinning::pin_dependencies(metadata, context.default_index(), platform).await?)
    }

    /// Infers a filename for the package bundle based on the identifier and platform, or the input path if no identifier is provided.
    fn infer_filename(&self, identifier: Option<&oci::Identifier>) -> String {
        let mut name = match identifier {
            Some(identifier) => format!("{}-{}", identifier.name(), identifier.tag_or_latest()),
            None => self
                .path
                .file_prefix()
                .and_then(|str| str.to_str())
                .unwrap_or("package")
                .to_string(),
        };
        if let Some(platform) = &self.platform {
            name.push_str(&format!("-{}", platform.ascii_segments().join("-")));
        }
        format!("{}.tar.xz", name)
    }
}
