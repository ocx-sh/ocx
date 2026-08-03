// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package cascade check` - read-only rolling-tag audit.

use std::process::ExitCode;

use clap::Parser;

use crate::options;

/// Report where a package's rolling tags disagree with its versions.
///
/// Reads every version tag the registry holds for each package, folds them
/// into the rolling-tag state the cascade rules imply, and reports each
/// difference: a rolling tag missing a platform, one pointing at outdated
/// content, one that was never created, and any entry nothing accounts for.
/// For an `ocx.sh/...` package it also compares the live public index with the
/// registry, so an entry that is stale there is reported too.
///
/// Read-only. It authenticates for pull only and never writes anything. Give a
/// tag (`cmake:3.28`) to narrow the audit to that part of the graph.
///
/// Exits 0 when everything agrees, 65 when anything does not, and 64 when a
/// package names a digest or a tag that is not a version.
#[derive(Parser)]
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
