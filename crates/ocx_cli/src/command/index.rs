// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Index {
    /// List available repositories in the registry
    Catalog(super::index_catalog::IndexCatalog),
    /// List available versions of a package
    List(super::index_list::IndexList),
    /// Updates available versions of a package.
    Update(super::index_update::IndexUpdate),
}

impl Index {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Index::Catalog(catalog) => catalog.execute(context).await,
            Index::List(list) => list.execute(context).await,
            Index::Update(update) => update.execute(context).await,
        }
    }
}
