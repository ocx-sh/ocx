use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci};

use crate::api;

#[derive(Parser)]
pub struct IndexCatalog {
    /// List tags for each repository in the catalog.
    #[clap(long)]
    with_tags: bool,
}

impl IndexCatalog {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let registry = context.default_registry();

        let repositories = context.default_index().list_repositories(&registry).await?;
        if !self.with_tags {
            let catalog = api::data::catalog::Catalog::without_tags(repositories);
            context.api().report_catalog(catalog)?;
            return Ok(ExitCode::SUCCESS);
        }

        let mut join_set = tokio::task::JoinSet::<anyhow::Result<(String, Vec<String>)>>::new();
        for repository in repositories {
            let identifier = oci::Identifier::new_registry(repository.clone(), registry.clone());
            let context = context.clone();
            join_set.spawn(async move {
                let tags = match context.default_index().list_tags(&identifier).await? {
                    Some(tags) => tags,
                    None => {
                        log::warn!("No tags found for repository '{}'.", identifier);
                        Vec::new()
                    }
                };
                Ok((identifier.repository().into(), tags))
            });
        }

        let mut tags = std::collections::HashMap::new();
        while let Some(result) = join_set.join_next().await {
            if let Ok(Ok((repository, repository_tags))) = result {
                tags.insert(repository, repository_tags);
            } else if let Ok(Err(e)) = result {
                log::error!("Error fetching tags for repository: {:?}", e);
            } else if let Err(e) = result {
                log::error!("Task panicked while fetching tags for repository: {:?}", e);
            }
        }

        let catalog = api::data::catalog::Catalog::with_tags(tags);
        context.api().report_catalog(catalog)?;
        Ok(ExitCode::SUCCESS)
    }
}
