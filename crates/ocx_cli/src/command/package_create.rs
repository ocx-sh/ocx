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
    /// Required whenever `--metadata` is given: it declares the platform the
    /// packaged content runs on, which cannot be read off the machine doing
    /// the build. Dependencies carrying no digest are resolved against the
    /// selected index to a platform manifest digest for this platform, the
    /// content tree is scanned under this platform's executable convention,
    /// and the value is recorded in the metadata sidecar; `ocx package push`
    /// and `ocx package test` default to the recorded value and reject a
    /// `--platform` that disagrees. Resolution honors `--remote`,
    /// `--offline`, and `--frozen`. Also used to infer the output filename.
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
    /// Requires `--platform`. Dependencies without a digest are pinned to
    /// that platform's manifest digests; the resolved sidecar is written next
    /// to the output bundle in canonical form.
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
        let declared_platform = self.declared_platform()?;

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
        let resolved_metadata = match self.metadata.as_deref().zip(declared_platform) {
            Some((metadata_source, platform)) => {
                let metadata = AuthoringMetadata::read_json(metadata_source).await?;
                let metadata = self.resolve_dependency_pins(metadata, &context, &platform).await?;
                let metadata = self.resolve_binaries(metadata, &platform).await?;
                // Validate the projection the publisher will actually push:
                // run the publish-time env/entrypoint checks against the
                // declared platform.
                package::metadata::ValidMetadata::try_from(metadata.to_published(&platform)?)?;
                // Record the platform dependency pins were resolved against
                // so `ocx package push`/`ocx package test` bind to it.
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

    /// The platform `--metadata` is compiled for: dependency pins resolve
    /// against it, the binaries scan applies its executable convention, and
    /// it is recorded in the sidecar for `ocx package push` / `ocx package
    /// test` to read back. `None` when no sidecar was supplied — `--platform`
    /// then only shapes the inferred output filename.
    ///
    /// There is no default. The host platform describes what the build
    /// machine *supplies*; the recorded platform describes what the packaged
    /// artifact *demands* — a static musl binary cross-built on a glibc host
    /// demands neither the host's libc nor its architecture. Guessing one
    /// from the other corrupts every downstream consumer of the recorded
    /// value, so an absent `--platform` is a usage error rather than a
    /// silent host default.
    fn declared_platform(&self) -> anyhow::Result<Option<oci::Platform>> {
        match (&self.metadata, &self.platform) {
            (None, _) => Ok(None),
            (Some(_), Some(platform)) => Ok(Some(platform.clone())),
            (Some(_), None) => Err(ocx_lib::cli::UsageError::new(
                "--platform (-p) is required with --metadata (-m); the sidecar records the platform \
                 the packaged content runs on, which cannot be inferred from the build host",
            )
            .into()),
        }
    }

    /// Resolve unpinned dependencies against the selected index for
    /// `platform`. Already-pinned dependencies pass through untouched (no
    /// network).
    async fn resolve_dependency_pins(
        &self,
        metadata: AuthoringMetadata,
        context: &crate::app::Context,
        platform: &oci::Platform,
    ) -> anyhow::Result<AuthoringMetadata> {
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

    /// `--metadata` without `--platform` must not fall back to the host
    /// platform. The host describes what the build machine supplies; the
    /// sidecar field describes what the artifact demands. A musl bundle
    /// cross-built on a glibc host would otherwise be recorded as demanding
    /// `libc.glibc`, and `ocx package push` / `ocx package test` bind to that
    /// recorded value — so the corruption is unrecoverable downstream.
    #[test]
    fn metadata_without_platform_is_rejected_instead_of_defaulting_to_the_host() {
        let create = PackageCreate::try_parse_from(["package-create", "-m", "metadata.json", "."]).expect("parse");
        let error = create
            .declared_platform()
            .expect_err("--metadata without --platform must not resolve to the host platform");
        let message = error.to_string();
        assert!(
            message.contains("--platform") && message.contains("--metadata"),
            "usage error must name both flags: {message}"
        );
    }

    /// The rejection above is a usage error (64), not a data error — the
    /// invocation is malformed, nothing was read or parsed yet. Classified
    /// through the same path `main` uses.
    #[test]
    fn metadata_without_platform_exits_with_usage_error() {
        let create = PackageCreate::try_parse_from(["package-create", "-m", "metadata.json", "."]).expect("parse");
        let error = create.declared_platform().expect_err("rejected above");
        assert_eq!(
            crate::app::classify_error(error.as_ref()),
            ocx_lib::cli::ExitCode::UsageError
        );
    }

    /// Control for the two tests above: with `--platform` given, the declared
    /// value is what gets recorded — verbatim, including a libc feature the
    /// build host does not have.
    #[test]
    fn declared_platform_is_the_flag_value_verbatim() {
        let create = PackageCreate::try_parse_from([
            "package-create",
            "-m",
            "metadata.json",
            "-p",
            "linux/amd64+libc.musl",
            ".",
        ])
        .expect("parse");
        let platform = create.declared_platform().expect("explicit platform is accepted");
        assert_eq!(
            platform.map(|platform| platform.to_string()),
            Some("linux/amd64+libc.musl".to_string())
        );
    }

    /// No sidecar, no recorded platform: `--platform` stays optional there,
    /// where it only shapes the inferred output filename.
    #[test]
    fn no_metadata_needs_no_platform() {
        let create = PackageCreate::try_parse_from(["package-create", "."]).expect("parse");
        assert!(
            create.declared_platform().expect("no sidecar is ok").is_none(),
            "without --metadata there is nothing to record a platform on"
        );
    }
}
