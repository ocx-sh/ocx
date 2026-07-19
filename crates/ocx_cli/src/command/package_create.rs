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
    /// Scan the content tree for executables the package puts on `PATH` to
    /// fill or verify the `binaries` metadata claim; see
    /// `--bin-scan`/`--no-bin-scan`.
    #[clap(flatten)]
    bin_scan: options::BinScan,
}

impl PackageCreate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        self.validate_bin_scan()?;

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
                let metadata = self.resolve_binaries(metadata, &platform).await?;
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

    /// Rejects an explicit `--bin-scan` given without `--metadata` (`-m`):
    /// the flag has nothing to verify, and silently no-op'ing would defeat
    /// its purpose as an explicit verification switch. `--no-bin-scan`
    /// without `--metadata` stays a harmless no-op — there is nothing to
    /// disable.
    fn validate_bin_scan(&self) -> anyhow::Result<()> {
        if self.bin_scan.mode() == options::BinScanMode::Verify && self.metadata.is_none() {
            return Err(ocx_lib::cli::UsageError::new(
                "--bin-scan requires --metadata (-m); nothing to verify without a metadata sidecar",
            )
            .into());
        }
        Ok(())
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

    /// Runs the create-time interface-binaries scan/fill/verify step
    /// against `self.path`'s content tree, per `self.bin_scan`'s resolved
    /// mode (`adr_declared_binaries_metadata.md` §2 / §2.1 ordering block).
    async fn resolve_binaries(
        &self,
        metadata: AuthoringMetadata,
        platform: &oci::Platform,
    ) -> anyhow::Result<AuthoringMetadata> {
        let mode = match self.bin_scan.mode() {
            options::BinScanMode::Auto => package::bin_scan::ScanMode::Auto,
            options::BinScanMode::Verify => package::bin_scan::ScanMode::Verify,
            options::BinScanMode::Off => package::bin_scan::ScanMode::Off,
        };
        Ok(package::bin_scan::resolve_binaries(&self.path, metadata, platform, mode).await?)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `--bin-scan` without `--metadata` has nothing to verify — Cluster 2
    /// (arch-Warn) flagged the prior behavior as a silent no-op that exits 0
    /// without scanning anything.
    #[test]
    fn bin_scan_without_metadata_is_rejected() {
        let create = PackageCreate::try_parse_from(["package-create", "--bin-scan", "."]).expect("parse");
        let err = create
            .validate_bin_scan()
            .expect_err("--bin-scan without --metadata must be a usage error");
        let message = err.to_string();
        assert!(
            message.contains("--bin-scan") && message.contains("--metadata"),
            "usage error must name both flags: {message}"
        );
    }

    /// `--bin-scan` with `--metadata` present has a declaration to verify.
    #[test]
    fn bin_scan_with_metadata_is_accepted() {
        let create =
            PackageCreate::try_parse_from(["package-create", "--bin-scan", "-m", "metadata.json", "."]).expect("parse");
        assert!(create.validate_bin_scan().is_ok());
    }

    /// `--no-bin-scan` without `--metadata` stays a harmless no-op: there is
    /// nothing to disable, so it is not an error.
    #[test]
    fn no_bin_scan_without_metadata_is_accepted() {
        let create = PackageCreate::try_parse_from(["package-create", "--no-bin-scan", "."]).expect("parse");
        assert!(create.validate_bin_scan().is_ok());
    }

    /// Neither flag (Auto mode) without `--metadata` is unaffected — Auto
    /// never verifies, only `--bin-scan` does.
    #[test]
    fn auto_mode_without_metadata_is_accepted() {
        let create = PackageCreate::try_parse_from(["package-create", "."]).expect("parse");
        assert!(create.validate_bin_scan().is_ok());
    }
}
