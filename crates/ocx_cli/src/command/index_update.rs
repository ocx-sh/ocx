// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, oci::index};

use crate::options;

#[derive(Parser)]
pub struct IndexUpdate {
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl IndexUpdate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // `ocx index update` refreshes tags AND persists their manifest
        // chain via `IndexSync::refresh_package` (wraps `LocalIndex::refresh_tags`,
        // `adr_index_indirection.md` Decision H): writes the tag → digest
        // pointer plus the verbatim, digest-verified manifest bytes into the
        // snapshot store's object CAS (A1/A3), so the committed snapshot
        // resolves fully offline afterwards.
        let remote_index = index::Index::from_remote(context.remote_index()?.clone());
        // Per-namespace static-file index sources, when online. A package in an
        // index-bearing namespace refreshes through the two-hop index path
        // rather than the registry (`adr_index_indirection.md` F5a — kind per
        // NAMESPACE); every other package refreshes against the registry.
        let index_sources = context.index_sources();
        let packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        // Tag each refresh with its input index so any failures can be surfaced
        // in input order. `IndexSync::refresh_package` returns `crate::Result<()>`
        // (not a PackageManager op), so `drain_package_tasks` does not fit; the
        // index-tagged fan-out is inlined here (same shape as `package info`).
        let mut join_set: tokio::task::JoinSet<(usize, ocx_lib::Result<()>)> = tokio::task::JoinSet::new();
        for (index, identifier) in packages.iter().enumerate() {
            // Route to the index source whose namespace serves this package, if
            // any; otherwise refresh against the registry.
            let source = index_sources
                .iter()
                .find(|source| source.namespace() == identifier.registry())
                .map(|source| index::Index::from_source(source.clone()))
                .unwrap_or_else(|| remote_index.clone());
            let context = context.clone();
            let identifier = identifier.clone();
            join_set.spawn(async move { (index, context.index_sync().refresh_package(&identifier, &source).await) });
        }

        let mut failures: Vec<(usize, ocx_lib::Error)> = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((_, Ok(()))) => {}
                Ok((index, Err(e))) => {
                    log::error!("Failed to update index for '{}': {e}", packages[index]);
                    failures.push((index, e));
                }
                Err(join_err) => {
                    // A refresh task panicked — abort the rest and propagate,
                    // matching the `install_all` JoinSet panic precedent.
                    join_set.abort_all();
                    std::panic::resume_unwind(join_err.into_panic());
                }
            }
        }

        // Any failure → return the input-order-first error so `classify_error`
        // (main.rs) derives a deterministic nonzero exit. No stdout report: this
        // is an action command with no payload; the aggregated error on stderr
        // is the batch signal.
        if !failures.is_empty() {
            failures.sort_by_key(|(index, _)| *index);
            let (_, error) = failures.into_iter().next().expect("failures is non-empty");
            return Err(error.into());
        }

        // ── Piggyback: sync the static-file index catalog when online. ──
        //
        // Conditional-GET `c/index.json`, re-snapshot only the packages whose
        // root digest moved, and persist the catalog map at `{index-home}/c/index.json`
        // — the offline catalog source and the next diff basis (F2). Best-effort:
        // an absent index (config.json 404) yields an empty catalog with no error,
        // and any transport failure warns rather than failing the tag refresh.
        for source in context.index_sources() {
            match context.index_sync().sync_catalog(source).await {
                Ok(outcome) => log::debug!(
                    "index update: catalog sync complete for '{}' ({} package(s) re-snapshotted)",
                    source.namespace(),
                    outcome.moved.len()
                ),
                Err(error) => log::warn!(
                    "index update: catalog sync for '{}' failed (non-fatal): {error}",
                    source.namespace()
                ),
            }
        }

        // ── Piggyback: refresh site-patch descriptors when the patch tier is active. ──
        //
        // After the tag index is refreshed, also re-fetch patch descriptors for all
        // known installed bases so the patch tier stays in sync with the rest of the
        // index. This is best-effort: a sync failure (offline, registry unreachable,
        // required-companion error) is logged as a warning and does NOT fail the
        // index-update command — the tag refresh is the primary job.
        //
        // Only runs when:
        //   1. A `[patches]` section is configured (manager.patches().is_some()), AND
        //   2. The manager is online (is_offline() is false — sync_patches checks
        //      this internally, but we skip the call entirely when offline to avoid
        //      the OfflineMode error allocation overhead).
        //
        // ADR decision: piggyback keeps descriptor metadata fresh after every index
        // refresh without requiring users to remember a separate `ocx patch sync`.
        if context.manager().patches().is_some() && !context.manager().is_offline() {
            let host = oci::Platform::current().unwrap_or_else(oci::Platform::any);
            match context.manager().sync_patches(&[host]).await {
                Ok(_report) => {
                    log::debug!("index update: patch descriptor sync completed");
                }
                Err(error) => {
                    log::warn!("index update: patch descriptor sync failed (non-fatal): {error}");
                }
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
