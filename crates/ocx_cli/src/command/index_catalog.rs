// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci};

use crate::api;

/// A per-repository tag-fetch outcome, tagged with its input index so failures
/// can be surfaced in input order.
type IndexedTagResult = (usize, ocx_lib::Result<(String, Vec<String>)>);

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

        // Index-tagged so a per-repository failure can be surfaced in input
        // order, matching `index update`'s fail-fast aggregation below.
        let mut join_set: tokio::task::JoinSet<IndexedTagResult> = tokio::task::JoinSet::new();
        for (index, repo) in repositories.iter().enumerate() {
            let identifier = oci::Identifier::new_registry(repo.repository(), repo.registry());
            let display_name = repo.to_string();
            let context = context.clone();
            join_set.spawn(async move {
                let result = context.default_index().list_tags(&identifier).await.map(|tags| {
                    let tags = tags.unwrap_or_else(|| {
                        log::warn!("No tags found for repository '{}'.", identifier);
                        Vec::new()
                    });
                    (display_name, tags)
                });
                (index, result)
            });
        }

        let mut tags = std::collections::BTreeMap::new();
        let mut failures: Vec<(usize, ocx_lib::Error)> = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((_, Ok((repository, repository_tags)))) => {
                    tags.insert(repository, repository_tags);
                }
                Ok((index, Err(e))) => {
                    log::error!("fetching tags for repository '{}' failed: {e:#}", repositories[index]);
                    failures.push((index, e));
                }
                Err(join_err) => {
                    // A tag-fetch task panicked — abort the rest and propagate,
                    // matching the `index update` JoinSet panic precedent.
                    join_set.abort_all();
                    std::panic::resume_unwind(join_err.into_panic());
                }
            }
        }

        // A per-repository tag fetch that errors (e.g. `--remote` against an
        // unreachable source) must surface as a nonzero exit rather than a
        // SUCCESS report with a partial or empty catalog — a script consuming
        // JSON output otherwise cannot tell "no tags" (`None`, handled above)
        // from "fetch failed". Matches `index update`'s fail-fast aggregation:
        // the input-order-first failure, deterministic across repeated runs.
        if !failures.is_empty() {
            failures.sort_by_key(|(index, _)| *index);
            let (_, error) = failures.into_iter().next().expect("failures is non-empty");
            return Err(error.into());
        }

        let catalog = api::data::catalog::Catalog::with_tags(tags.into_iter().collect());
        context.api().report(&catalog)?;
        Ok(ExitCode::SUCCESS)
    }
}
