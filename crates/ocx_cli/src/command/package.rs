// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Package {
    /// Creates an archive from a local package directory.
    Create(super::package_create::PackageCreate),
    /// Downloads packages into the local object store without creating install symlinks.
    Pull(super::package_pull::PackagePull),
    Push(super::package_push::PackagePush),
}

impl Package {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Package::Create(create) => create.execute(context).await,
            Package::Pull(pull) => pull.execute(context).await,
            Package::Push(deploy) => deploy.execute(context).await,
        }
    }
}
