use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    log, oci,
    package::{self, version::Version},
    prelude::*,
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

    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,

    #[clap(short, long, required = true)]
    platform: oci::Platform,

    identifier: options::Identifier,
    content: std::path::PathBuf,
}

impl PackagePush {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;
        let metadata = match &self.metadata {
            Some(path) => path.clone(),
            None => crate::conventions::infer_metadata_file(&self.content)?,
        };

        let mut versions_to_push = Vec::new();
        if self.cascade {
            let version = match Version::from_str(identifier.tag_or_latest()) {
                Some(version) => version,
                None => {
                    return Err(anyhow::anyhow!(
                        "Identifier tag is not a valid version, cannot cascade: {}",
                        identifier.tag_or_latest()
                    ));
                }
            };
            let other_tags = match context.remote_client()?.list_tags(identifier.clone()).await {
                Ok(tags) => tags,
                Err(err) => {
                    if self.new {
                        log::info!(
                            "Failed to list existing tags for package {}, assuming this is a new package: {err}",
                            identifier
                        );
                        Vec::new()
                    } else {
                        return Err(anyhow::anyhow!(
                            "Failed to list existing tags for package {}: {err}",
                            identifier
                        ));
                    }
                }
            };
            let other_versions = other_tags
                .into_iter()
                .filter_map(|tag| Version::from_str(&tag))
                .collect::<Vec<_>>();
            let (cascaded_versions, is_latest) = version.cascade(other_versions);
            versions_to_push = cascaded_versions
                .into_iter()
                .map(|v| identifier.clone_with_tag(v.to_string()))
                .collect();
            if is_latest {
                versions_to_push.push(identifier.clone_with_tag("latest".to_string()));
            }
        } else {
            versions_to_push.push(identifier.clone());
        }

        log::info!(
            "Deploying package from {} with metadata {}",
            self.content.display(),
            metadata.display()
        );
        let metadata = package::metadata::Metadata::read_json_from_path(&metadata)?;
        let package_info = package::info::Info {
            identifier: identifier.clone(),
            metadata,
            platform: self.platform.clone(),
        };

        if let Some(identifier) = versions_to_push.first() {
            log::info!("Pushing package with identifier {}", identifier);
            let (digest, manifest) = context
                .remote_client()?
                .push_package(package_info.clone(), self.content.clone())
                .await?;
            let source_identifier = identifier.clone_with_digest(digest);
            for identifier in versions_to_push {
                context
                    .remote_client()?
                    .copy_manifest_data(&manifest, &source_identifier, identifier.tag_or_latest())
                    .await?;
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
