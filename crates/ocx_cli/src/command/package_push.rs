// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, package, prelude::*, publisher::Publisher};

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
        let metadata_path = match &self.metadata {
            Some(path) => path.clone(),
            None => crate::conventions::infer_metadata_file(&self.content)?,
        };

        log::info!(
            "Deploying package from {} with metadata {}",
            self.content.display(),
            metadata_path.display()
        );
        let metadata = package::metadata::Metadata::read_json(&metadata_path).await?;
        let info = package::info::Info {
            identifier: identifier.clone(),
            metadata,
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
            publisher.push_cascade(info, &self.content, existing_versions).await?;
        } else {
            publisher.push(info, &self.content).await?;
        }

        Ok(ExitCode::SUCCESS)
    }
}
