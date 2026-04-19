// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use futures::stream::{self, StreamExt};
use ocx_lib::{log, oci::index};

use crate::options;

/// Maximum concurrent registry tag refreshes when running `index update`.
///
/// Each task hits the remote registry once per identifier. The cap matches
/// `index catalog --tags` (`CATALOG_TAG_FETCH_CONCURRENCY = 8`) — both are
/// network-bound interactive commands with the same registry rate-limit
/// budget. `.buffered` preserves input order, so error logs land in
/// package-list order instead of arrival order.
const INDEX_UPDATE_CONCURRENCY: usize = 8;

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

        let results = stream::iter(packages.iter())
            .map(|identifier| {
                let remote_index = remote_index.clone();
                let context = context.clone();
                let identifier = identifier.clone();
                async move { context.local_index().refresh_tags(&identifier, &remote_index).await }
            })
            .buffered(INDEX_UPDATE_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        for result in results {
            if let Err(e) = result {
                log::error!("Failed to update index for a package: {e}");
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
