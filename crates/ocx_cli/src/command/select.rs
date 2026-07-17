// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::{collections::HashMap, process::ExitCode};

use clap::Parser;

use crate::{api, conventions, options};

/// Set the current version of one or more packages.
///
/// Resolves each package via the index and verifies that its content is present
/// in the local object store, then updates the per-repo `current` symlink to
/// point at the package root (consumers traverse `<current>/content/`,
/// `<current>/entrypoints/`, or `<current>/metadata.json`). The same per-repo
/// `.select.lock` that `install --select` acquires is held here as well, and
/// closure-scoped entrypoint name collisions surface at consumption time
/// (`ocx env`, `ocx exec`) rather than at select. No downloading is performed.
#[derive(Parser)]
pub struct Select {
    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Package identifiers to select.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Select {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let platform = conventions::platform_or_default(self.platform.platform.clone());

        // `select_all` resolves in parallel (via `find_all`) then wires each
        // `current` symlink sequentially, aggregating every per-package failure
        // into one `SelectFailed` instead of aborting on the first. Results are
        // returned in input order, so zipping with `self.packages` is sound.
        let results = context.manager().select_all(identifiers, platform).await?;

        let mut packages = HashMap::with_capacity(results.len());

        for (raw, (info, outcome)) in self.packages.iter().zip(results.iter()) {
            packages.insert(
                raw.raw().to_string(),
                api::data::install::InstallEntry {
                    identifier: info.identifier().clone().into(),
                    metadata: info.metadata().clone(),
                    path: outcome.current.clone(),
                },
            );
        }

        context.api().report(&api::data::install::Installs::new(packages))?;

        Ok(ExitCode::SUCCESS)
    }
}
