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
                        // Same `repositories` vector as the `error!` below, so
                        // the same neutralization. `warn!` reaches stderr under
                        // the default INFO console filter.
                        log::warn!(
                            "No tags found for repository '{}'.",
                            api::data::sanitize_for_terminal(&identifier.to_string())
                        );
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
                    // Both halves are foreign-authored and neither reaches
                    // `main.rs`'s boundary intact: `repositories` comes from
                    // `list_repositories`' response, and the aggregation below
                    // returns the lowest-index failure alone, so every other
                    // chain is printed here and nowhere else. Without this the
                    // command neutralized the same repository name on stdout
                    // (`api::data::catalog`) while emitting it raw on stderr.
                    log::error!(
                        "fetching tags for repository '{}' failed: {}",
                        api::data::sanitize_for_terminal(&repositories[index].to_string()),
                        api::data::sanitize_error_chain(&e)
                    );
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

#[cfg(test)]
mod tests {
    /// Both stderr sites here print names from `list_repositories`, and neither
    /// is covered by `main.rs`'s boundary: the aggregation returns the
    /// lowest-index failure alone, and the `warn!` is not a failure at all. This
    /// command neutralized the same names on stdout while emitting them raw on
    /// stderr until `1dea4f78`, so the pairing is the contract.
    #[test]
    fn every_stderr_site_is_neutralized() {
        let body = include_str!("index_catalog.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half");
        assert_eq!(
            body.matches("log::warn!").count() + body.matches("log::error!").count(),
            2,
            "a third stderr site here needs its own neutralization and this count updated"
        );
        assert_eq!(
            body.matches("log::error!").count(),
            body.matches("sanitize_error_chain(&").count(),
            "every error log must render its chain through `sanitize_error_chain`"
        );
        for interpolation in ["identifier", "repositories[index]"] {
            assert!(
                body.contains(&format!("sanitize_for_terminal(&{interpolation}")),
                "`{interpolation}` reaches stderr and must route through the sanitizer"
            );
        }
        for raw in ["{e:#}", "{:#}", "{:?}"] {
            assert!(
                !body.contains(raw),
                "`{raw}` interpolates an error without the sanitizer or without its cause chain"
            );
        }
        assert!(!body.contains("eprintln!"), "error prose goes through the log boundary");
    }
}
