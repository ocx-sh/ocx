// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Package {
    /// Creates an archive from a local package directory.
    Create(super::package_create::PackageCreate),
    /// Push a description (README + optional logo) to a package repository.
    Describe(super::package_describe::PackageDescribe),
    /// Show description metadata for a package repository.
    Info(super::package_info::PackageInfo),
    /// Downloads packages into the local object store without creating install symlinks.
    Pull(super::package_pull::PackagePull),
    Push(super::package_push::PackagePush),
    /// Sign a published package's manifest (keyless Sigstore, via OCI Referrers).
    Sign(super::package_sign::PackageSign),
    /// Materialize a package locally (no registry round-trip) and run a command in its env.
    Test(super::package_test::PackageTest),
    /// Verify a published package's Sigstore signature (keyless, via OCI Referrers).
    Verify(super::verify::Verify),
}

impl Package {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Package::Create(create) => create.execute(context).await,
            Package::Describe(describe) => describe.execute(context).await,
            Package::Info(info) => info.execute(context).await,
            Package::Pull(pull) => pull.execute(context).await,
            Package::Push(deploy) => deploy.execute(context).await,
            Package::Sign(sign) => sign.execute(context).await,
            Package::Test(test) => test.execute(context).await,
            Package::Verify(verify) => verify.execute(context).await,
        }
    }
}
