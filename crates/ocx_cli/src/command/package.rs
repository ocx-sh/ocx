// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Package {
    Push(super::package_push::PackagePush),
    /// Creates an archive from a local package directory.
    Create(super::package_create::PackageCreate),
}

impl Package {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Package::Push(deploy) => deploy.execute(context).await,
            Package::Create(create) => create.execute(context).await,
        }
    }
}
