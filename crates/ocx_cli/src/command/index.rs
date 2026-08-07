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
    /// Run it without `--frozen`: recording a new tag-to-digest mapping is
    /// the discovery a freeze exists to refuse, so `--frozen` rejects this
    /// command with exit 81 instead of moving any pin.
    ///
    /// A tagged identifier (`cmake:3.28`) records only that tag; a bare
    /// identifier (`cmake`) records every tag.
    ///
    /// Only the packages you name are touched: every other package keeps
    /// the version it already resolves to. There is no whole-index sync: a
    /// remote index floats, and the local copy is the set of snapshots you
    /// asked for. Packages with an update waiting are reported afterward.
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
