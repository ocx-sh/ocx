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
/// The toolchain-tier counterparts (`ocx env`, `ocx exec`) remain at root.
///
/// `Announce` and `Claim` carry **no** doc comment on purpose, the same way
/// [`CascadeGroup`](super::package_cascade::CascadeGroup)'s variants do: clap
/// renders a variant's own doc as the subcommand's help and ignores the
/// argument struct's whenever one is present, so a doc here would silently
/// replace the multi-paragraph text those two commands need — which is exactly
/// how both shipped with a `--help` that named no credential. The user-facing
/// text lives with the flags it describes, in `package_announce.rs` and
/// `package_claim.rs`, and
/// `package_announce::tests::both_write_commands_render_the_credential_guidance`
/// asserts over the *rendered* help so a doc re-added here reds rather than
/// silently winning.
#[derive(Subcommand)]
pub enum Package {
    Announce(super::package_announce::PackageAnnounce),
    /// Attach an in-toto attestation to a published package manifest.
    ///
    /// Signs a caller-supplied predicate document (an SBOM, provenance,
    /// VEX) with the same keyless Sigstore machinery `sign` uses, and
    /// publishes it as an OCI referrer on the target manifest.
    Attest(super::package_attest::PackageAttest),
    /// Audit and repair the rolling tags a published package cascades into.
    ///
    /// Pushing `3.28.1` is supposed to leave `3.28`, `3` and `latest` pointing
    /// at it for every platform it ships. `check` reports where that is no
    /// longer true; `repair` re-points the tags using content the registry
    /// already holds.
    #[command(subcommand)]
    Cascade(super::package_cascade::CascadeGroup),
    Claim(super::package_claim::PackageClaim),
    /// Promote an already-published package to another registry or repository.
    ///
    /// Copies the platform manifests, their blobs and their referrers verbatim,
    /// so the digest never moves and every signature and lock pin naming it
    /// still resolves. The target's index is merged one platform at a time,
    /// never overwritten.
    Copy(super::package_copy::PackageCopy),
    /// Creates an archive from a local package directory.
    Create(super::package_create::PackageCreate),
    /// Publish or fetch a package repository's catalog description.
    #[command(subcommand)]
    Description(super::package_description::DescriptionGroup),
    /// Deprecated spelling of `ocx package description push`; removed in 0.7.
    #[command(name = "describe", hide = true)]
    DeprecatedDescribe(super::package_description_push::PackageDescriptionPush),
    /// Deprecated spelling of `ocx package description pull`; removed in 0.7.
    #[command(name = "info", hide = true)]
    DeprecatedInfo(super::package_description_pull::PackageDescriptionPull),
    /// Show the dependency tree for one or more installed packages.
    Deps(super::deps::Deps),
    /// Print the resolved environment variables for one or more installed packages.
    Env(super::env::Env),
    /// Inspect one or more package references (candidates, metadata, or resolution chain).
    Inspect(super::package_inspect::PackageInspect),
    /// Install packages from a local or remote index (no `ocx.toml` touched).
    Install(super::install::Install),
    /// Downloads packages into the local object store without creating install symlinks.
    Pull(super::package_pull::PackagePull),
    /// Publish a package's layers and metadata to a registry.
    Push(super::package_push::PackagePush),
    /// List or extract the SBOM attestations a published package carries.
    Sbom(super::package_sbom::PackageSbom),
    /// Set the current version of one or more packages.
    Select(super::select::Select),
    /// Remove the current-version symlink for one or more packages.
    Deselect(super::deselect::Deselect),
    /// Sign a published package's manifest (keyless Sigstore, via OCI Referrers).
    Sign(super::package_sign::PackageSign),
    /// Materialize a package locally (no registry round-trip) and run a command in its env.
    Test(super::package_test::PackageTest),
    /// Verify a published package's Sigstore signature (keyless, via OCI Referrers).
    Verify(super::package_verify::PackageVerify),
    /// Runs installed packages.
    #[command(visible_alias = "x")]
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
            Package::Announce(announce) => announce.execute(context).await,
            Package::Attest(attest) => attest.execute(context).await,
            Package::Cascade(cascade) => cascade.execute(context).await,
            Package::Claim(claim) => claim.execute(context).await,
            Package::Copy(copy) => copy.execute(context).await,
            Package::Create(create) => create.execute(context).await,
            Package::Description(description) => description.execute(context).await,
            Package::DeprecatedDescribe(push) => {
                super::deprecated::warn_renamed(&context, "package describe", "package description push");
                push.execute(context).await
            }
            Package::DeprecatedInfo(pull) => {
                super::deprecated::warn_renamed(&context, "package info", "package description pull");
                pull.execute(context).await
            }
            Package::Deps(deps) => deps.execute(context).await,
            Package::Env(env) => env.execute(context).await,
            Package::Inspect(inspect) => inspect.execute(context).await,
            Package::Install(install) => install.execute(context).await,
            Package::Pull(pull) => pull.execute(context).await,
            Package::Push(deploy) => deploy.execute(context).await,
            Package::Sbom(sbom) => sbom.execute(context).await,
            Package::Select(select) => select.execute(context).await,
            Package::Deselect(deselect) => deselect.execute(context).await,
            Package::Sign(sign) => sign.execute(context).await,
            Package::Test(test) => test.execute(context).await,
            Package::Verify(verify) => verify.execute(context).await,
            Package::Exec(exec) => exec.execute(context).await,
            Package::Uninstall(uninstall) => uninstall.execute(context).await,
            Package::Which(which) => which.execute(context).await,
        }
    }
}
