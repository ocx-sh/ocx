// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci::index};

use crate::options;

#[derive(Parser)]
pub struct IndexUpdate {
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl IndexUpdate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let remote_index = index::Index::from_remote(context.remote_index()?.clone());
        let packages =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let mut join_set = tokio::task::JoinSet::new();
        for identifier in &packages {
            let remote_index = remote_index.clone();
            let mut context = context.clone();
            let identifier = identifier.clone();
            join_set.spawn(async move { context.local_index_mut().update(&remote_index, &identifier).await });
        }

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(())) => (),
                Ok(Err(e)) => log::error!("Failed to update index for a package: {e}"),
                Err(e) => log::error!("Task panicked while updating index for a package: {e}"),
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
