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
    #[command(long_about = "\
        Freeze companion package digests to a snapshot for reproducible builds.\n\n\
        Resolves every companion and descriptor digest in the active patch overlay and writes a \
        `patches.snapshot.json` file beside `ocx.lock`. Once frozen, setting `OCX_PATCH_SNAPSHOT` \
        to that path makes all env composition prefer the pinned digests over live tag lookups.\n\n\
        Works offline: only the local object store is consulted.\n\n\
        Adopting a snapshot is a deliberate opt-in, independent of `--frozen` (which scopes to \
        the package tier). Companions are pinned per `repository:tag`, so a descriptor naming one \
        repository at two tags freezes both versions independently.\n\n\
        Under `--global` the file is written beside `$OCX_HOME/ocx.lock`; the flag goes before \
        the subcommand.")]
    Freeze(super::patch_freeze::PatchFreezeArgs),

    /// Refresh patch descriptors and companion packages from the registry.
    #[command(long_about = "\
        Refresh patch descriptors and companion packages from the registry.\n\n\
        Re-fetches every patch descriptor for all installed packages and the global descriptor. \
        Installs any newly-referenced companion packages.\n\n\
        Requires network access.\n\n\
        This command also picks up patches for packages installed before patch configuration was \
        added. All states are re-checked regardless of what was previously recorded.\n\n\
        The scope is every installed package, the tools locked in every known project's \
        `ocx.lock`, and the global descriptor. It is to the patch tier what `ocx index update` is \
        to the index, not what `ocx index sync` is.\n\n\
        Other commands re-check descriptors they have already seen (`install`, `pull`, `lock`, \
        `add`, `update`, `package test`) or fetch only unseen ones (`exec`, `env`). Under a \
        `required` tier a descriptor that was present and is now gone fails them with exit 79 \
        until this command records it as absent. Under `OCX_PATCH_SNAPSHOT` none of them \
        re-checks anything.\n\n\
        Without `--platform` it resolves every concrete ship platform (linux/amd64, linux/arm64, \
        darwin/amd64, darwin/arm64, windows/amd64, windows/arm64), not only the host. The synced set therefore covers \
        whatever a teammate might run offline. Pass one `--platform` to narrow it.")]
    Sync(super::patch_sync::PatchSyncArgs),

    /// Publish a patch descriptor to the patch registry.
    #[command(long_about = "\
        Publish a patch descriptor to the patch registry.\n\n\
        Reads a descriptor JSON file and validates it. Then it pushes the descriptor to the configured patch \
        registry. The target is either the reserved global repository (`--global`) or the package-specific \
        sub-path for a given base identifier.\n\n\
        The descriptor only references companion packages by identifier. Publish the companion \
        packages separately with `ocx package push`.\n\n\
        Requires network access; fails in offline mode.\n\n\
        `--registry HOST/PATH` overrides the configured `[patches]` tier for this command, which \
        bootstraps a new patch registry without a config block. The descriptor is validated \
        before any network work, so a malformed file fails with its path named.")]
    Publish(super::patch_publish::PatchPublishArgs),

    /// Compose a patch descriptor onto a base locally, without publishing.
    #[command(long_about = "\
        Compose a patch descriptor onto a base locally, without publishing.\n\n\
        Reads a descriptor JSON file and composes its matched companions onto the given base \
        identifier in a scratch store. It then runs a test script, runs a trailing command \
        in the composed environment, or prints the composed environment. Composes the surface \
        a consumer of the base sees, or the base's own with `--self`.\n\n\
        Lets a maintainer verify a descriptor before publishing it.\n\n\
        Required companion packages must be resolvable (installed locally or pulled from the \
        registry); an unresolvable required companion fails the command. An optional companion \
        that cannot be resolved is warned about and skipped, matching production fail-open.\n\n\
        Nothing is published and `$OCX_HOME` is not modified. With `--script`, a Starlark test \
        script runs against the composed environment.")]
    Test(super::patch_test::PatchTestArgs),

    /// Show which companion contributes each patched env var to a base, and
    /// by which descriptor rule.
    #[command(long_about = "\
        Show which companion contributes each patched env var to a base, and by which descriptor \
        rule.\n\n\
        Resolves the base identifier directly and lists every env var the configured patch \
        registry overlays onto it. Each row names the descriptor rule glob that matched and the companion \
        identifier that produced the var. Traces the surface a consumer of the base sees, or the \
        base's own with `--self`.\n\n\
        A base with no applicable patch reports an empty result, not an error; a companion that \
        applies but adds no var is named instead.\n\n\
        It never consults `ocx.toml`: the base is resolved against the configured `[patches]` \
        registry. Output follows the root `--format` flag, a `Variable | Rule | Companion` table \
        in plain form and an `items` array of `variable`, `rule`, `companion` objects in JSON.")]
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
