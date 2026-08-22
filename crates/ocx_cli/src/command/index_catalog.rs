// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci};

use crate::api;

/// A per-repository tag-fetch outcome, tagged with its input index so failures
/// can be surfaced in input order.
type IndexedTagResult = (usize, ocx_lib::Result<(String, Vec<String>)>);

/// How many per-repository tag listings `--tags` keeps in flight.
///
/// Its own permit class, deliberately never `index_common`'s
/// `INDEX_REFRESH_CONCURRENCY`: #316 and #167 both record that an inner
/// fan-out reusing the *same* class deadlocks, because an ancestor holds a
/// permit while waiting on children that cannot acquire one. This loop is
/// top-level today and has no such ancestor, so sharing would be safe *here* —
/// a separate constant makes the rule hold by construction instead of by an
/// argument about the current call graph, which a later refactor can falsify.
///
/// Unlike the refresh fan-out this number is the in-flight request count
/// outright: one tag listing is a single latency-bound round trip with no
/// nested fan-out beneath it, so nothing multiplies it. 16 clears a
/// several-hundred-repository catalog in a handful of rounds while staying far
/// below the `index` verb family's stated 512-request ceiling.
const CATALOG_TAG_CONCURRENCY: usize = 16;

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
        //
        // A `JoinSet` gated on a permit rather than `index_common`'s
        // `buffer_unordered`: the join below reads `JoinError::is_panic()` to
        // abort the rest and re-raise. A stream has no task boundary, so a
        // panic would unwind the caller directly, and "a task panic still
        // aborts the rest and propagates" (`adr_index_sync_performance.md`
        // S-008) would have to be restated to describe that instead.
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(CATALOG_TAG_CONCURRENCY));
        let mut join_set: tokio::task::JoinSet<IndexedTagResult> = tokio::task::JoinSet::new();
        for (index, repo) in repositories.iter().enumerate() {
            let identifier = oci::Identifier::new_registry(repo.repository(), repo.registry());
            let display_name = repo.to_string();
            let context = context.clone();
            // Acquired before the spawn, so a registry listing thousands of
            // repositories holds that many permits' worth of work rather than
            // that many live tasks. Nothing closes `permits` — it is owned by
            // this frame and outlives the loop.
            let permit = std::sync::Arc::clone(&permits)
                .acquire_owned()
                .await
                .expect("the semaphore is owned by this frame and is never closed");
            join_set.spawn(async move {
                // Released when the task ends, however it ends: a panic unwinds
                // through it and `abort_all` drops it, so neither can strand a
                // permit and wedge the loop.
                let _permit = permit;
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
    use super::CATALOG_TAG_CONCURRENCY;

    /// This module's PRODUCTION half with comment lines stripped — the window
    /// every source-text guard here counts over.
    ///
    /// Comments go first, or a denylist matches the comment that quotes the
    /// forms it forbids. The non-vacuity check is against the window being too
    /// WIDE, not too narrow: a truncated window fails the positive counts below
    /// loudly, but a window that swallowed the test module would let these very
    /// assertions' own string literals stand in for the production code they are
    /// about. `mod tests {` appears nowhere else in the file, so it is the tell.
    fn production_half() -> String {
        let production = include_str!("index_catalog.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half");
        let window = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            window.contains("async fn execute"),
            "the scanned window must reach the code the guards are about"
        );
        assert!(
            !window.contains("mod tests {"),
            "the window must stop before the test module, or every count is polluted by tests"
        );
        window
    }

    /// C-022's bound is a Rust-side contract, not only an acceptance one.
    ///
    /// This module is one of the two **exemptions** in
    /// `index_common.rs::no_index_module_outside_this_one_grows_a_refresh_fan_out`,
    /// and that guard justifies exempting this file by asserting this file
    /// carries a bound of its own on its own permit class. Until this test
    /// existed the only thing behind that claim was a Docker-dependent
    /// acceptance test `task rust:verify` never runs — so the exemption rested
    /// on nothing a Rust-side reviewer could see fail. This is the test the
    /// exemption's comment points at.
    ///
    /// Three assertions, three different failures: a literal or arithmetic width
    /// leaves the constant reading true while the ceiling moves; a permit taken
    /// inside the spawned task bounds nothing at all (every repository gets a
    /// live task and queues from inside it); and the acceptance test restates
    /// the number as a Python literal, which is asymmetric on its own — raising
    /// the constant past its threshold reds it, lowering the constant only
    /// weakens the assertion, silently.
    #[test]
    fn the_catalog_tag_fan_out_is_bounded_by_the_constant() {
        let production = production_half();

        assert_eq!(
            production.matches("Semaphore::new(").count(),
            1,
            "one permit class governs this loop; a second needs its own review against C-022"
        );
        assert_eq!(
            production.matches("Semaphore::new(CATALOG_TAG_CONCURRENCY)").count(),
            1,
            "the width must come from the constant, not a literal"
        );

        let acquire = production
            .find(".acquire_owned()")
            .expect("the permit is what makes the bound real");
        let spawn = production
            .find("join_set.spawn(")
            .expect("non-vacuity: there is a spawn for the permit to be ahead of");
        assert!(
            acquire < spawn,
            "the permit is acquired BEFORE the spawn: taken inside the task, a registry listing \
             thousands of repositories gets thousands of live tasks and the semaphore bounds only \
             how many are mid-request"
        );

        // The acceptance test hand-copies this number (it is Rust, that suite is
        // not) and derives its threshold from the copy, so a LOWERED constant
        // lowers the threshold with it and the measurement silently stops
        // measuring the shipped bound. Read the copy rather than restating the
        // literal a third time: this reds in both directions, and it reds here,
        // in `task rust:verify`, without Docker.
        let acceptance_test = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/tests/test_index.py");
        let acceptance = std::fs::read_to_string(&acceptance_test)
            .unwrap_or_else(|error| panic!("{} pairs with this constant: {error}", acceptance_test.display()));
        let restated = format!("CATALOG_TAG_CONCURRENCY = {CATALOG_TAG_CONCURRENCY}");
        assert!(
            acceptance.contains(&restated),
            "`{}` must restate this constant as `{restated}` — its in-flight threshold is derived \
             from the copy, so the two drifting apart makes the end-to-end measurement assert \
             something other than the bound this module ships",
            acceptance_test.display()
        );
    }

    /// Both stderr sites here print names from `list_repositories`, and neither
    /// is covered by `main.rs`'s boundary: the aggregation returns the
    /// lowest-index failure alone, and the `warn!` is not a failure at all. This
    /// command neutralized the same names on stdout while emitting them raw on
    /// stderr until `1dea4f78`, so the pairing is the contract.
    #[test]
    fn every_stderr_site_is_neutralized() {
        let body = production_half();
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
