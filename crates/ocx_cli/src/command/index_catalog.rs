// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use futures::stream::{self, StreamExt};
use ocx_lib::{log, oci};

use crate::api;

/// Maximum concurrent registry tag fetches when expanding `index catalog --tags`.
///
/// Each fetch hits the registry once per repository. The cap keeps interactive
/// `ocx index catalog --tags` runs well under typical registry rate-limit
/// budgets (Docker Hub anonymous pull is 100 req/6h per IP) while still being
/// large enough that a dev-internal registry catalog of dozens of repos
/// completes in roughly the same wall time as the previous unbounded fan-out.
const CATALOG_TAG_FETCH_CONCURRENCY: usize = 8;

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
            vec![context.default_registry()]
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

        // Fetch tags with bounded concurrency. `repositories` was sorted at
        // line 35 and `.buffered` preserves submission order, so the collected
        // vec is already in display-name order — no separate reorder step.
        let collected: Vec<(String, Result<Option<Vec<String>>, _>)> = stream::iter(repositories.iter())
            .map(|repo| {
                let identifier = oci::Identifier::new_registry(repo.repository(), repo.registry());
                let display_name = repo.to_string();
                let context = context.clone();
                async move {
                    let result = context.default_index().list_tags(&identifier).await;
                    (display_name, result)
                }
            })
            .buffered(CATALOG_TAG_FETCH_CONCURRENCY)
            .collect()
            .await;

        let mut tags: Vec<(String, Vec<String>)> = Vec::with_capacity(collected.len());
        for (display_name, result) in collected {
            match result {
                Ok(Some(repo_tags)) => tags.push((display_name, repo_tags)),
                Ok(None) => {
                    log::warn!("No tags found for repository '{display_name}'.");
                    tags.push((display_name, Vec::new()));
                }
                Err(e) => {
                    log::error!("Error fetching tags for repository '{display_name}': {e:?}");
                }
            }
        }

        let catalog = api::data::catalog::Catalog::with_tags(tags.into_iter().collect());
        context.api().report(&catalog)?;
        Ok(ExitCode::SUCCESS)
    }
}
