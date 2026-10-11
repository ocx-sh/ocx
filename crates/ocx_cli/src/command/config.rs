// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx config` — corporate managed-configuration commands (`adr_managed_config_tier.md`).
//!
//! `test` and `push` share `validate_managed_config_payload`, so the pre-publish check is what
//! publishing enforces.

use std::process::ExitCode;

use clap::Subcommand;

/// Manage the corporate managed-configuration tier (`[managed]`).
///
/// Decoupled from `ocx self update`: this tier tracks an operator-published
/// `config.toml` artifact, not the ocx binary itself.
///
/// The payload is synced from an OCI registry and merged above the user config
/// on every invocation. It travels as an ordinary OCX package whose content is
/// one `config.toml`, so publishing, versioning, cascade tags and rollbacks
/// behave exactly like packages.
#[derive(Subcommand)]
pub enum ConfigGroup {
    /// Adopt (or clear) the corporate managed-config tier.
    #[command(long_about = "\
        Adopt (or clear) the corporate managed-config tier.\n\n\
        The configuration-only counterpart to `ocx self setup --managed-config`: resolves the \
        source (flag, then `OCX_MANAGED_CONFIG`, then the existing seed), synchronously fetches \
        and persists a snapshot, then writes the `[managed]` seed fence. Installs no binary and \
        touches no shell profile - built for automation and CI environments.\n\n\
        The source resolves with the same precedence as `ocx self setup --managed-config`, and \
        the adoption sequence is identical. The snapshot is fetched and persisted first, and the \
        seed fence is written only on success. A fetch failure during first adoption, or while \
        self-healing a wiped or mismatched snapshot, leaves no partial state and fails.\n\n\
        A bare re-run against an adopted seed reconciles it every time: a newer digest replaces \
        the snapshot (`refreshed`), unchanged content confirms it (`already_adopted`). That \
        re-sync is best-effort: a fetch failure warns on stderr, keeps the snapshot and exits 0 \
        (`refresh_unavailable`).\n\n\
        A bare run with nothing configured at any level is a usage error (exit 64), unlike `ocx \
        self setup`, where an unresolved source is a no-op.\n\n\
        The `managed_config` status is `adopted`, `already_adopted`, `refreshed`, \
        `refresh_unavailable`, `cleared`, `dirty`, `would_adopt` or `would_refresh`.\n\n\
        Details: <https://ocx.sh/docs/reference/command-line/config/#config-setup>")]
    Setup(super::config_setup::ConfigSetupArgs),

    /// Refresh the managed-config snapshot from the registry.
    #[command(long_about = "\
        Refresh the managed-config snapshot from the registry.\n\n\
        Syncs the configured source (or an explicit VERSION - tag, digest, or `tag@digest` - for \
        pins and rollbacks), always bypassing the background-refresh throttle. `--pause \
        <duration>` holds the background tick for up to 7 days; `--resume` clears the pause and \
        syncs; `--check` reports status (drift, pause, pin) without fetching or swapping. \
        Without a VERSION, `--pause` freezes the on-disk state with no fetch; with one it syncs \
        first and records the pause only once the sync persists. An update without `--pause` \
        clears an active pause.\n\n\
        The snapshot identity is the repository, not the tag, so a pinned snapshot still \
        satisfies the `required` gate of a seed tracking a floating tag. A pause affects only the \
        background tick, never the `required` gate or an explicit update.\n\n\
        `--check` never modifies the pause file, and probes only the registry's top-level \
        manifest digest. Offline or on any probe failure it degrades to a local-state report \
        (`check_unavailable`) rather than claiming the tier is current.\n\n\
        Without `--check` the outcome is `not_configured` (no source resolved), `already_current` \
        (digest matches), `updated` (new snapshot persisted) or `check_unavailable` (`--pause` \
        without a VERSION fetched nothing).\n\n\
        `--check` reports `checked` when the probe found drift. A status report also lists \
        `kill_switches`, the active `OCX_NO_CONFIG_REFRESH` or `OCX_NO_CONFIG` names.\n\n\
        Details: <https://ocx.sh/docs/reference/command-line/config/#config-update>")]
    Update(super::config_update::ConfigUpdateArgs),

    /// Check a config file and preview the configuration it would produce.
    #[command(long_about = "\
        Check a config file and preview the configuration it would produce.\n\n\
        Runs the same checks `ocx config push` runs before publishing (parses as an ocx \
        config, carries no `[managed]` section, at most 64 KiB). It reports the configuration \
        this machine would resolve once the payload is adopted: default registry, registries, \
        mirrors and the `[patches]` tier. It also reports the machine's own `[managed]` posture. \
        Keys this ocx does not recognize are listed as warnings and do not fail the run. They \
        are equally typos and settings a newer ocx understands.\n\n\
        Keys inside a `[mirrors]` entry are not checked.\n\n\
        Validates locally; nothing is published, adopted, or written. Exits 78 when the file \
        is not a publishable payload.\n\n\
        The preview reproduces the adoption fold order: this machine's discovered tiers first, \
        then the candidate, then any explicit `--config` / `OCX_CONFIG` overlay. The \
        machine's current managed snapshot is never used, because the candidate stands in for it. An overlay wins \
        over the candidate only for the keys it sets.\n\n\
        The merged result also passes the gates every invocation applies. An invalid \
        `[mirrors.\"<host>\"]` entry fails the command. That means an unparseable URL, or plain \
        HTTP for a host the candidate does not declare insecure. An empty `[patches] registry` \
        fails the command too.\n\n\
        `--format json` always carries `candidate`, `valid`, `registries`, `mirrors`, `plain_http` \
        and `unknown_keys`, each `[]` when empty. `registry_default`, `patches` and `managed` \
        appear only when their tier is configured.")]
    Test(super::config_test::ConfigTestArgs),

    /// Publish a config file as a managed-config package.
    #[command(long_about = "\
        Publish a config file as a managed-config package.\n\n\
        Validates the payload (must parse as an ocx config, must not contain a `[managed]` \
        section, at most 64 KiB) and stages it as `config.toml`. It then pushes the payload as an ordinary \
        package, so fleets adopt it via `ocx self setup --managed-config` and `ocx config update`.\n\n\
        The payload can never redirect the tier that fetches it, hence no `[managed]` section. It \
        is bundled as a tar+gzip layer and pushed with the machinery of `ocx package push`.\n\n\
        With `--cascade`, pushing `user-1.4.2` also advances `user-1.4`, `user-1` and `user`, so \
        fleets track a floating tag while hosts can still pin any version.")]
    Push(super::config_push::ConfigPushArgs),
}

impl ConfigGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            ConfigGroup::Setup(args) => args.execute(context).await,
            ConfigGroup::Update(args) => args.execute(context).await,
            ConfigGroup::Test(args) => args.execute(context).await,
            ConfigGroup::Push(args) => args.execute(context).await,
        }
    }
}
