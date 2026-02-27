use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{compression, log, oci, package};

use crate::options;

#[derive(Parser)]
pub struct PackageCreate {
    /// Path to the package to bundle
    path: std::path::PathBuf,
    /// Optional identifier for the package, used to infer the output filename if not specified
    #[clap(short, long)]
    identifier: Option<options::Identifier>,
    /// Optional platform of the package, used to infer the output filename if not specified
    #[clap(short, long)]
    platform: Option<oci::Platform>,
    /// Output file or directory, if a directory is provided the filename will be inferred
    #[clap(short, long)]
    output: Option<std::path::PathBuf>,
    /// Force overwrite of output file if it already exists
    #[clap(short, long)]
    force: bool,
    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,
    /// Compression level to use for the package bundle
    #[arg(short = 'l', long, value_enum, default_value_t = options::CompressionLevel::Default)]
    compression_level: options::CompressionLevel,
}

impl PackageCreate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = options::Identifier::transform_optional(self.identifier.clone(), context.default_registry())?;
        let output = match &self.output {
            Some(output) => {
                if output.is_dir() || output.ends_with("/") {
                    output.join(self.infer_filename(&identifier))
                } else {
                    output.clone()
                }
            }
            None => self.infer_filename(&identifier).into(),
        };

        if output.exists() {
            if !self.force {
                anyhow::bail!(
                    "Output file {} already exists. Use --force to overwrite.",
                    output.display()
                );
            }
            std::fs::remove_file(&output)?;
        } else if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let compression_options = compression::CompressionOptions::from_level(self.compression_level.into());
        log::info!(
            "Creating package bundle from {} with compression level {:?}",
            self.path.display(),
            self.compression_level
        );
        package::bundle::BundleBuilder::from(&self.path)
            .with_compression(compression_options)
            .create(&output)
            .await?;
        log::info!(
            "Created package bundle from {} at {}",
            self.path.display(),
            output.display()
        );

        if let Some(metadata_source) = &self.metadata {
            let metadata_target = crate::conventions::infer_metadata_file(&output)?;
            std::fs::copy(metadata_source, &metadata_target)?;
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Infers a filename for the package bundle based on the identifier and platform, or the input path if no identifier is provided.
    fn infer_filename(&self, identifier: &Option<oci::Identifier>) -> String {
        let mut name = match identifier {
            Some(identifier) => format!(
                "{}-{}",
                identifier.name().unwrap_or("package".to_string()),
                identifier.tag_or_latest()
            ),
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
