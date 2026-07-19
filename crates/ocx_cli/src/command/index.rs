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
    /// Refresh the local index for one or more packages
    ///
    /// Fetches the requested packages' tags from the registry (or a
    /// configured index source) and records tag-to-digest mappings in the
    /// local index, so `--offline` and `--frozen` resolution works
    /// afterward without contacting the network. Does not download the
    /// package itself - use `ocx package install` or `ocx package pull`
    /// for that.
    ///
    /// A tagged identifier (`cmake:3.28`) records only that tag; a bare
    /// identifier (`cmake`) records every tag.
    ///
    /// If any package fails to refresh, the whole command fails; packages
    /// that refresh successfully keep their updated tags. See
    /// [index update](https://ocx.sh/docs/reference/command-line#index-update)
    /// for details.
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
