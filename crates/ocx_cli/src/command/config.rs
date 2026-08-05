// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx config` — corporate managed-configuration commands.
//!
//! Four verbs: `setup` (adopt or clear the `[managed]` tier — the
//! config-only counterpart to `ocx self setup --managed-config`, sharing the
//! same lib implementation), `update` (fetch + persist the managed-config
//! snapshot, or `--check` for a probe-only report), `test` (report-only
//! validation plus effective-merge preview of a candidate payload) and `push`
//! (operator-side publish of a `config.toml` payload as an ordinary ocx
//! package — managed-config v2, ADR `adr_managed_config_tier.md` v2
//! amendment).
//!
//! `test` and `push` share `validate_managed_config_payload`, so the check a
//! maintainer runs before publishing is the one publishing enforces.

use std::process::ExitCode;

use clap::Subcommand;

/// Manage the corporate managed-configuration tier (`[managed]`).
///
/// Decoupled from `ocx self update`: this tier tracks an operator-published
/// `config.toml` artifact, not the ocx binary itself.
#[derive(Subcommand)]
pub enum ConfigGroup {
    /// Adopt (or clear) the corporate managed-config tier.
    ///
    /// The configuration-only counterpart to `ocx self setup
    /// --managed-config`: resolves the source (flag, then
    /// `OCX_MANAGED_CONFIG`, then the existing seed), synchronously fetches
    /// and persists a snapshot, then writes the `[managed]` seed fence.
    /// Installs no binary and touches no shell profile - built for
    /// automation and CI environments.
    ///
    /// Details: <https://ocx.sh/docs/reference/command-line#config-setup>
    Setup(super::config_setup::ConfigSetupArgs),

    /// Refresh the managed-config snapshot from the registry.
    ///
    /// Syncs the configured source (or an explicit VERSION - tag, digest, or
    /// `tag@digest` - for pins and rollbacks), always bypassing the
    /// background-refresh throttle. `--pause <duration>` holds the background
    /// tick for up to 7 days; `--resume` clears the pause and syncs; `--check`
    /// reports status (drift, pause, pin) without fetching or swapping.
    ///
    /// Details: <https://ocx.sh/docs/reference/command-line#config-update>
    Update(super::config_update::ConfigUpdateArgs),

    /// Check a config file and preview the configuration it would produce.
    ///
    /// Runs the same checks `ocx config push` runs before publishing (parses
    /// as an ocx config, carries no `[managed]` section, at most 64 KiB) and
    /// reports the configuration this machine would resolve once the payload
    /// is adopted: default registry, registries, mirrors and the `[patches]`
    /// tier, alongside the machine's own `[managed]` posture. Keys this ocx
    /// does not recognize are listed as warnings and do not fail the run -
    /// they are equally typos and settings a newer ocx understands. Keys
    /// inside a `[mirrors]` entry are not checked.
    ///
    /// Validates locally; nothing is published, adopted, or written. Exits 78
    /// when the file is not a publishable payload.
    Test(super::config_test::ConfigTestArgs),

    /// Publish a config file as a managed-config package.
    ///
    /// Validates the payload (must parse as an ocx config, must not contain
    /// a `[managed]` section, at most 64 KiB), stages it as `config.toml`,
    /// and pushes it as an ordinary package so fleets adopt it via
    /// `ocx self setup --managed-config` and `ocx config update`.
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
