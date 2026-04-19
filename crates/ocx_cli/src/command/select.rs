// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::{collections::HashMap, process::ExitCode};

use clap::Parser;
use ocx_lib::oci;

use crate::{api, conventions::platforms_or_default, options};

/// Set the current version of one or more packages.
///
/// Resolves each package via the index and verifies that its content is present
/// in the local object store, then updates the per-repo `current` symlink to
/// point at the package root (consumers traverse `<current>/content/`,
/// `<current>/entrypoints/`, or `<current>/metadata.json`). The same
/// per-registry collision check, per-repo `.select.lock`, and
/// `entrypoints-index.json` ownership write that `install --select` runs are
/// applied here — `install --select` and `select` share a single code path.
/// No downloading is performed.
#[derive(Parser)]
pub struct Select {
    /// Platforms to consider when resolving the package. Defaults to the current platform.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to select.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Select {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let platforms = platforms_or_default(&self.platforms);

        let package_infos = context.manager().find_all(identifiers.clone(), platforms).await?;

        let mut packages = HashMap::with_capacity(package_infos.len());

        for ((raw, identifier), info) in self.packages.iter().zip(identifiers.iter()).zip(package_infos.iter()) {
            // Drive the same wire-selection pipeline as `install --select`:
            // collision check + atomic pair update + index publish under a
            // shared per-registry lock.
            let outcome = context.manager().wire_selection(identifier, info, false, true).await?;

            packages.insert(
                raw.raw().to_string(),
                api::data::install::InstallEntry {
                    identifier: info.identifier.clone().into(),
                    metadata: info.metadata.clone(),
                    path: outcome.current,
                },
            );
        }

        context.api().report(&api::data::install::Installs::new(packages))?;

        Ok(ExitCode::SUCCESS)
    }
}
