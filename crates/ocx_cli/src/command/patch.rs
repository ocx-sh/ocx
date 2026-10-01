// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx [--global] patch` — patch overlay management commands.

use std::process::ExitCode;

use clap::Subcommand;

/// Manage patch overlays for a project.
///
/// Commands here read or write `patches.snapshot.json`, which pins companion
/// package digests for reproducible builds. Use `patch freeze` to write the
/// snapshot and `patch sync` to refresh patch descriptors and companions from
/// the registry.
#[derive(Subcommand)]
pub enum PatchGroup {
    /// Freeze companion package digests to a snapshot for reproducible builds.
    ///
    /// Resolves every companion and descriptor digest in the active patch
    /// overlay and writes a `patches.snapshot.json` file beside `ocx.lock`.
    /// Once frozen, setting `OCX_PATCH_SNAPSHOT` to that path makes all env
    /// composition prefer the pinned digests over live tag lookups.
    ///
    /// Works offline: only the local object store is consulted.
    Freeze(super::patch_freeze::PatchFreezeArgs),

    /// Refresh patch descriptors and companion packages from the registry.
    ///
    /// Re-fetches every patch descriptor for all installed packages and the
    /// global descriptor. Installs any newly-referenced companion packages.
    /// Requires network access.
    ///
    /// This command also picks up patches for packages installed before patch
    /// configuration was added. All states are re-checked regardless of what
    /// was previously recorded.
    Sync(super::patch_sync::PatchSyncArgs),

    /// Publish a patch descriptor to the patch registry.
    ///
    /// Reads a descriptor JSON file, validates it, and pushes it to the
    /// configured patch registry under either the reserved global repository
    /// (`--global`) or the package-specific sub-path for a given base
    /// identifier.
    ///
    /// The descriptor only references companion packages by identifier. Publish
    /// the companion packages separately with `ocx package push`. Requires
    /// network access; fails in offline mode.
    Publish(super::patch_publish::PatchPublishArgs),

    /// Compose a patch descriptor onto a base locally, without publishing.
    #[command(long_about = "\
        Compose a patch descriptor onto a base locally, without publishing.\n\n\
        Reads a descriptor JSON file and composes its matched companions onto the given base \
        identifier in a scratch store, then either runs a test script, runs a trailing command in \
        the composed environment, or prints the composed environment. Composes the surface a \
        consumer of the base sees, or the base's own with `--self`. Lets a maintainer verify a \
        descriptor before publishing it.\n\n\
        Required companion packages must be resolvable (installed locally or pulled from the \
        registry); an unresolvable required companion fails the command.")]
    Test(super::patch_test::PatchTestArgs),

    /// Show which companion contributes each patched env var to a base, and
    /// by which descriptor rule.
    ///
    /// Resolves the base identifier directly and lists every env var the
    /// configured patch registry overlays onto it, naming the descriptor rule
    /// glob that matched and the companion identifier that produced the var.
    /// Traces the surface a consumer of the base sees, or the base's own with
    /// `--self`. A base with no applicable patch reports an empty result, not
    /// an error; a companion that applies but adds no var is named instead.
    Why(super::patch_why::PatchWhyArgs),
}

impl PatchGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            PatchGroup::Freeze(args) => args.execute(context).await,
            PatchGroup::Sync(args) => args.execute(context).await,
            PatchGroup::Publish(args) => args.execute(context).await,
            PatchGroup::Test(args) => args.execute(context).await,
            PatchGroup::Why(args) => args.execute(context).await,
        }
    }
}
