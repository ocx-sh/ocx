// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package cascade check` - read-only rolling-tag audit.

use std::process::ExitCode;

use clap::Parser;

use crate::options;

/// Report where a package's rolling tags disagree with its versions.
#[derive(Parser)]
#[command(long_about = "\
    Report where a package's rolling tags disagree with its versions.\n\n\
    Reads every version tag the registry holds for each package and folds them into the \
    rolling-tag state the cascade rules imply. It reports each difference. That covers a rolling tag \
    missing a platform, one pointing at outdated content, one that was never created, and any \
    entry nothing accounts for. For an `ocx.sh/...` package it also compares the live public \
    index with the registry, so an entry that is stale there is reported too.\n\n\
    Read-only. It authenticates for pull only and never writes anything.\n\n\
    Give a tag (`cmake:3.28`) to narrow the audit to that part of the graph.\n\n\
    Exits 0 when everything agrees, 65 when anything does not, and 64 when a package names a \
    digest or a tag that is not a version.\n\n\
    Nothing re-checks the cascade graph if a cascade push is interrupted partway through. \
    `check` finds that drift afterwards without republishing anything, and never writes to the \
    registry or any local index. Identifiers may be logical or physical.\n\n\
    A logical one (a namespace with an index configured, such as `ocx.sh/kitware/cmake:3.28`) \
    resolves to its registry location as `ocx package install` does. It also fetches the \
    namespace's live index root. That adds a third finding layer, index staleness, which compares each \
    observed alias digest with the digest committed in the index. A physical one \
    (`ghcr.io/ocx-contrib/cmake:3.28`) skips that layer.\n\n\
    The tag on an identifier selects the scope. No tag audits every alias tag in every variant \
    track. `:latest` audits only the default variant's track.\n\n\
    A rolling tag such as `:3.28` or `:debug-3` audits that tag's subtree plus the path up to its \
    root. A fully build-tagged leaf audits only the path up to the root. The leaf is the source \
    of truth and never a write target.\n\n\
    A bare variant name (`:debug`) and a digest reference are usage errors (exit 64). \
    Identifiers naming the same package union their scopes into one report.")]
pub struct PackageCascadeCheck {
    /// Packages to audit.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl PackageCascadeCheck {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let audits = super::package_cascade::audit_all(&context, &self.packages).await?;

        let skipped = super::package_cascade::index_layer_skipped(&audits);
        let mut report = crate::api::data::package_cascade_check::PackageCascadeCheck::new(
            audits.into_iter().map(|audit| audit.report).collect(),
        );
        report.index_layer_skipped = skipped;
        context.api().report(&report)?;

        Ok(crate::conventions::cascade_check_exit_code(&report).into())
    }
}
