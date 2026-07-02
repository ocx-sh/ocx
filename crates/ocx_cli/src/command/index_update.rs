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
        // `ocx index update` is strictly a tag refresh: it locks the
        // tag → digest pointer into the local index file without persisting
        // any manifest blobs. Install-time ChainedIndex write-through owns
        // the manifest chain persistence contract.
        let remote_index = index::Index::from_remote(context.remote_index()?.clone());
        let packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        // Tag each refresh with its input index so any failures can be surfaced
        // in input order. `refresh_tags` returns `crate::Result<()>` (not a
        // PackageManager op), so `drain_package_tasks` does not fit; the
        // index-tagged fan-out is inlined here (same shape as `package info`).
        let mut join_set: tokio::task::JoinSet<(usize, ocx_lib::Result<()>)> = tokio::task::JoinSet::new();
        for (index, identifier) in packages.iter().enumerate() {
            let remote_index = remote_index.clone();
            let context = context.clone();
            let identifier = identifier.clone();
            join_set.spawn(async move {
                (
                    index,
                    context.local_index().refresh_tags(&identifier, &remote_index).await,
                )
            });
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
