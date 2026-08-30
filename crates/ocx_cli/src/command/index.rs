// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Index {
    /// List available repositories in the registry
    Catalog(super::index_catalog::IndexCatalog),
    /// List available versions of a package
    #[command(visible_alias = "ls")]
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
    /// the version it already resolves to. The local copy is the set of
    /// snapshots you asked for, never a mirror of a floating remote:
    /// `ocx index sync <REGISTRY>` is the same work over a registry's whole
    /// catalog, and is equally explicit.
    ///
    /// If any package fails to refresh, the whole command fails; packages
    /// that refresh successfully keep their updated tags.
    ///
    /// Details: <https://ocx.sh/docs/reference/command-line#index-update>
    Update(super::index_update::IndexUpdate),
    /// Refresh every package one or more registries' catalogs list
    ///
    /// The whole-registry form of `ocx index update`: instead of naming
    /// packages, name the registries, and each one's own catalog names the
    /// packages. This is how a whole mirror is snapshotted in one command.
    ///
    /// The set is read from the source live (a published source's own
    /// `c/index.json`, a plain-OCI registry's repository listing), never from
    /// the local copy, which is the set you already have. Each package is
    /// refreshed as if named bare, so it adopts every tag the source lists and
    /// keeps any tag only the local copy holds.
    ///
    /// It is a union of snapshots, never a replica: a merge never deletes, so a
    /// package that has disappeared upstream keeps the tags this machine already
    /// recorded. `ocx index regenerate` is the only command that drops anything,
    /// and only a catalog entry whose root document is already gone; it cannot
    /// retract a package, for which the answer is a fresh index home.
    ///
    /// Every `<REGISTRY>` is enumerated before any is refused, so one
    /// unreachable source does not cost the others their snapshot; the command
    /// still fails afterwards, reporting each failure. A source that answers but
    /// serves no catalog document at all is a failure too, never an empty set:
    /// a catalog listing zero packages is the only clean way to snapshot
    /// nothing.
    ///
    /// The refresh is bounded at the same in-flight ceiling `ocx index update`
    /// uses, across all registries together rather than per registry.
    ///
    /// Recording a new tag-to-digest mapping is the discovery a freeze exists to
    /// refuse, so `--frozen` rejects this command with exit 81, and `--offline`
    /// rejects it for contacting the source, including with `--dry-run`, which
    /// still enumerates.
    ///
    /// Details: <https://ocx.sh/docs/reference/command-line#index-sync>
    Sync(super::index_sync::IndexSync),
    /// Rebuild a local index source's catalog from the root documents on disk
    ///
    /// `c/index.json` is derived data: every entry restates a digest the root
    /// document beside it already carries. Every other writer only ever adds to
    /// it, which leaves one drift nothing can repair: an entry naming a package
    /// whose root document is gone. This re-derives the whole map from the
    /// tree, so such an entry is dropped.
    ///
    /// It reports, per registry, the number of root documents found and every
    /// package it added, corrected or removed; a run that changed nothing says
    /// so in one line.
    ///
    /// It consults no source and moves no pin, so `--frozen` and `--offline`
    /// both permit it. It writes exactly one file per registry, that registry's
    /// `c/index.json`, and never `config.json`, whose `name_segments` is an
    /// operator declaration no tree can be read for. It also clears a stale
    /// `c/index.json.etag` beside it if one is present, which matters if your
    /// served tree tracks that file in version control.
    ///
    /// Each `<REGISTRY>` must be a published index source (one configured with
    /// `[registries."<ns>"] index`). A derived, plain-OCI namespace has no
    /// catalog document by grammar (its catalog is the package enumeration
    /// itself) and is refused.
    ///
    /// It enumerates neither symlinked root documents nor symlinked directories
    /// under `p/`. Because the catalog is replaced wholesale, running it against
    /// a symlink-deduplicated layout drops every package reached through a link:
    /// a symlinked directory takes everything beneath it in one step.
    Regenerate(super::index_regenerate::IndexRegenerate),
}

impl Index {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Index::Catalog(catalog) => catalog.execute(context).await,
            Index::List(list) => list.execute(context).await,
            Index::Update(update) => update.execute(context).await,
            Index::Sync(sync) => sync.execute(context).await,
            Index::Regenerate(regenerate) => regenerate.execute(context).await,
        }
    }
}
