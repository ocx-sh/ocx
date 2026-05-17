// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// OCI-tier package commands.
///
/// These commands operate on OCI identifiers directly and never consult
/// `ocx.toml`.  They own the `candidate`/`current` floating symlinks.
///
/// The former root commands `ocx install`, `ocx uninstall`, `ocx select`,
/// `ocx exec`, and `ocx deselect` are moved here (C1 — handshake §2 / §7).
/// The toolchain-tier counterparts (`ocx env`, `ocx run`) remain at root.
#[derive(Subcommand)]
pub enum Package {
    /// Creates an archive from a local package directory.
    Create(super::package_create::PackageCreate),
    /// Push a description (README + optional logo) to a package repository.
    Describe(super::package_describe::PackageDescribe),
    /// Print the resolved environment variables for one or more installed packages.
    Env(super::env::Env),
    /// Show description metadata for a package repository.
    Info(super::package_info::PackageInfo),
    /// Inspect a package's metadata and (with --resolve) its resolution chain.
    Inspect(super::package_inspect::PackageInspect),
    /// Install packages from a local or remote index (no `ocx.toml` touched).
    Install(super::install::Install),
    /// Downloads packages into the local object store without creating install symlinks.
    Pull(super::package_pull::PackagePull),
    Push(super::package_push::PackagePush),
    /// Set the current version of one or more packages.
    Select(super::select::Select),
    /// Remove the current-version symlink for one or more packages.
    Deselect(super::deselect::Deselect),
    /// Materialize a package locally (no registry round-trip) and run a command in its env.
    Test(super::package_test::PackageTest),
    /// Runs installed packages.
    Exec(super::exec::Exec),
    /// Remove an installed candidate for one or more packages.
    Uninstall(super::uninstall::Uninstall),
    /// Resolve installed packages and print their package-root (or, with
    /// `--candidate`/`--current`, install-symlink) paths.
    Which(super::which::Which),
}

impl Package {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Package::Create(create) => create.execute(context).await,
            Package::Describe(describe) => describe.execute(context).await,
            Package::Env(env) => env.execute(context).await,
            Package::Info(info) => info.execute(context).await,
            Package::Inspect(inspect) => inspect.execute(context).await,
            Package::Install(install) => install.execute(context).await,
            Package::Pull(pull) => pull.execute(context).await,
            Package::Push(deploy) => deploy.execute(context).await,
            Package::Select(select) => select.execute(context).await,
            Package::Deselect(deselect) => deselect.execute(context).await,
            Package::Test(test) => test.execute(context).await,
            Package::Exec(exec) => exec.execute(context).await,
            Package::Uninstall(uninstall) => uninstall.execute(context).await,
            Package::Which(which) => which.execute(context).await,
        }
    }
}
