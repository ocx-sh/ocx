use std::{collections::HashMap, process::ExitCode};

use clap::Parser;
use ocx_lib::{log, oci};

use crate::{api, options};

#[derive(Parser)]
pub struct IndexList {
    /// Includes platforms available for each tag in the report.
    #[arg(long)]
    with_platforms: bool,

    /// Package identifiers to list the available versions for.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl IndexList {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;
        let tags_report = self
            .packages
            .iter()
            .zip(identifiers.clone().into_iter())
            .map(|(package, identifier)| {
                let context = context.clone();
                async move {
                    let tags = match context.default_index().list_tags(&identifier).await? {
                        Some(tags) => tags,
                        None => {
                            log::warn!("Package '{}' not found in the index.", identifier);
                            Vec::new()
                        }
                    };
                    Ok((package.raw().to_string(), tags.into_iter()))
                }
            });

        if !self.with_platforms {
            let tags_report = futures::future::join_all(tags_report)
                .await
                .into_iter()
                .collect::<anyhow::Result<Vec<_>>>()?;
            let tags_report = tags_report.into_iter().collect::<std::collections::HashMap<_, _>>();
            context
                .api()
                .report_tags(api::data::tag::Tags::without_platforms(tags_report))?;
            return Ok(ExitCode::SUCCESS);
        }

        let mut join_set = tokio::task::JoinSet::<Result<(String, HashMap<String, Vec<String>>), anyhow::Error>>::new();
        let tags_report = futures::future::join_all(tags_report).await;
        for (tags, identifier) in tags_report.into_iter().zip(identifiers.iter()) {
            let (package, tags) = tags?;
            let identifier = identifier.clone();
            let context = context.clone();
            join_set.spawn(async move {
                let mut platform_tags = HashMap::<_, Vec<_>>::new();
                for tag in tags {
                    let Some((_, manifest)) = context.default_index().fetch_manifest(&identifier).await? else {
                        log::warn!("Manifest not found for tag '{}' of '{}' — skipping.", tag, identifier);
                        continue;
                    };
                    let platforms = oci::Platform::from_manifest(&manifest)?;
                    for platform in platforms {
                        platform_tags
                            .entry(platform.to_string())
                            .or_insert_with(Vec::new)
                            .push(tag.clone());
                    }
                }
                Ok((package, platform_tags))
            });
        }

        let mut tags_report = HashMap::new();
        while let Some(result) = join_set.join_next().await {
            if let Ok(Ok((package, platform_tags))) = result {
                tags_report.insert(package, platform_tags);
            } else if let Ok(Err(e)) = result {
                log::error!("Error fetching platforms for package: {:?}", e);
            } else if let Err(e) = result {
                log::error!("Task panicked while fetching platforms for package: {:?}", e);
            }
        }

        context
            .api()
            .report_tags(api::data::tag::Tags::with_platforms(tags_report))?;
        Ok(ExitCode::SUCCESS)
    }
}
