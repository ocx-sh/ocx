// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci};

use crate::api;

#[derive(Parser)]
pub struct IndexCatalog {
    /// List tags for each repository in the catalog.
    #[clap(long)]
    tags: bool,

    /// Registries to list repositories from (defaults to OCX_DEFAULT_REGISTRY).
    #[arg(value_name = "REGISTRY")]
    registries: Vec<String>,
}

impl IndexCatalog {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let registries = if self.registries.is_empty() {
            vec![context.default_registry().to_string()]
        } else {
            self.registries.clone()
        };

        let mut repositories = Vec::new();
        for registry in &registries {
            let repos = context.default_index().list_repositories(registry).await?;
            repositories.extend(repos.into_iter().map(|r| oci::Repository::new(registry, r)));
        }
        repositories.sort();

        if !self.tags {
            let names = repositories.iter().map(|r| r.to_string()).collect();
            let catalog = api::data::catalog::Catalog::without_tags(names);
            context.api().report(&catalog)?;
            return Ok(ExitCode::SUCCESS);
        }

        let mut join_set = tokio::task::JoinSet::<anyhow::Result<(String, Vec<String>)>>::new();
        for repo in &repositories {
            let identifier = oci::Identifier::new_registry(repo.repository(), repo.registry());
            let display_name = repo.to_string();
            let context = context.clone();
            join_set.spawn(async move {
                let tags = match context.default_index().list_tags(&identifier).await? {
                    Some(tags) => tags,
                    None => {
                        log::warn!("No tags found for repository '{}'.", identifier);
                        Vec::new()
                    }
                };
                Ok((display_name, tags))
            });
        }

        let mut tags = std::collections::BTreeMap::new();
        while let Some(result) = join_set.join_next().await {
            if let Ok(Ok((repository, repository_tags))) = result {
                tags.insert(repository, repository_tags);
            } else if let Ok(Err(e)) = result {
                log::error!("fetching tags for repository failed: {e:?}");
            } else if let Err(e) = result {
                log::error!("task panicked while fetching tags for repository: {e:?}");
            }
        }

        let catalog = api::data::catalog::Catalog::with_tags(tags.into_iter().collect());
        context.api().report(&catalog)?;
        Ok(ExitCode::SUCCESS)
    }
}
