// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx config setup` — config-only managed-config adoption.
//!
//! The automation/CI counterpart to `ocx self setup --managed-config`: adopts
//! (or clears) the corporate managed-config tier without bootstrapping the ocx
//! binary, writing env shims, or touching shell profiles. Both entry points
//! share the single lib implementation (`ocx_lib::setup::apply_managed_config`),
//! so precedence, the fetch-first ordering, and the dirty-fence contract are
//! identical by construction.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::{self, ExitCode as OcxExitCode};

use crate::api::data::config_setup::ConfigSetupData;

/// Adopt (or clear) the corporate managed-config tier.
///
/// Resolves an OCI reference to a managed-config artifact, synchronously
/// fetches and persists a snapshot, and only then writes the `[managed]`
/// seed fence in `$OCX_HOME/config.toml` - a fetch failure leaves no
/// partial state. Pass an empty string (`--managed-config ""`) to clear an
/// existing seed and delete the snapshot.
///
/// Precedence when the flag is omitted: `OCX_MANAGED_CONFIG` env var, then
/// the existing seed (a bare run re-adopts and self-heals a wiped or
/// mismatched snapshot). Nothing resolved at any level is a usage error -
/// unlike `ocx self setup`, this command exists only to set up the tier.
///
/// Unlike `ocx self setup`, no binary is installed and no shell profile is
/// touched - this is the configuration-only entry point for automation and
/// CI environments.
///
/// # Exit codes
///
/// | Outcome | Exit |
/// |---|---|
/// | adopted / already adopted / cleared / would adopt | 0 |
/// | nothing to set up (no flag, no env var, no seed) | 64 |
/// | the fetched managed-config package is malformed (digest mismatch, no `any/any` entry, missing `config.toml`, over 64 KiB, or invalid TOML) | 65 |
/// | the registry is unreachable while fetching the snapshot | 69 |
/// | writing the snapshot or the `[managed]` fence failed | 74 |
/// | invalid ref, or a system-locked tier would be redirected/cleared | 78 |
/// | package not found in registry | 79 |
/// | authentication failed while fetching the snapshot | 80 |
/// | the `[managed]` fence carries user edits (no `--force`) | 82 |
#[derive(Parser)]
pub struct ConfigSetupArgs {
    /// Adopt (or clear) this managed-config source.
    ///
    /// An OCI reference to a managed-config artifact (published with
    /// `ocx config push`). Pass an empty string (`--managed-config ""`) to
    /// clear an existing seed and delete the snapshot. Omit to fall back to
    /// `OCX_MANAGED_CONFIG`, then the existing seed.
    #[arg(long, value_name = "REF")]
    managed_config: Option<String>,

    /// Report the intended actions without writing anything.
    #[arg(long)]
    dry_run: bool,

    /// Overwrite a `[managed]` fence that carries user edits (the dirty state).
    #[arg(long)]
    force: bool,
}

/// Resolve the effective `--managed-config` value the lib layer consumes.
///
/// The lib contract is deliberately simple: `Some(ref)` adopts, `Some("")`
/// clears, `None` leaves the tier untouched. This CLI seam owns the precedence
/// the lib no longer applies — when the flag is omitted it falls back to the
/// same runtime `[managed]` resolution [`ocx_lib::resolve_managed_target`]
/// uses (`OCX_MANAGED_CONFIG` non-empty over the `[managed].source` seed,
/// honoring a system-lock). An explicit flag — including `--managed-config ""`
/// to clear — is passed through verbatim.
///
/// Shared by `ocx config setup` and `ocx self setup` (which re-adopts a
/// configured seed on a bare re-run so the lib's Current-fence arm can
/// self-heal a wiped or mismatched snapshot).
///
/// A system-locked tier refuses an explicit value that would clear or redirect
/// it (exit 78) — a lock only tightens, and clearing/refetching would otherwise
/// corrupt the required tier the lock protects.
pub fn resolve_managed_config_arg(
    flag: Option<&str>,
    config: &ocx_lib::Config,
    env_override: Option<&str>,
) -> anyhow::Result<Option<String>> {
    if let Some(value) = flag {
        ocx_lib::check_locked_managed_override(config, value)?;
        return Ok(Some(value.to_string()));
    }
    Ok(ocx_lib::resolve_managed_target(config, env_override)?.map(|resolved| resolved.source.to_string()))
}

impl ConfigSetupArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let resolved = resolve_managed_config_arg(
            self.managed_config.as_deref(),
            context.config(),
            context.managed_config_env_override(),
        )?;

        // Nothing resolved anywhere: unlike `self setup` (which has other
        // phases to run and treats this as a no-op), an explicit
        // `config setup` with nothing to set up is a usage error.
        let Some(value) = resolved else {
            return Err(cli::UsageError::new(
                "nothing to set up: pass --managed-config <REF>, set OCX_MANAGED_CONFIG, \
                 or configure a [managed] seed",
            )
            .into());
        };

        let outcome = ocx_lib::setup::apply_managed_config(
            context.config(),
            Some(&value),
            self.dry_run,
            self.force,
            context.manager(),
            context.file_structure(),
        )
        .await?;

        // The dirty-fence contract mirrors `self setup`: left untouched
        // without --force → exit 82 so scripts can `case $? in 82)`.
        // Dry-run reports would-adopt and never returns 82.
        let dirty = matches!(outcome, ocx_lib::setup::ManagedConfigSetupOutcome::Dirty);
        let exit = if dirty && !self.force && !self.dry_run {
            OcxExitCode::DirtyRcBlock.into()
        } else {
            ExitCode::SUCCESS
        };

        context.api().report(&ConfigSetupData::from_outcome(&outcome))?;
        Ok(exit)
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_managed_config_arg;

    /// Builds a `Config` carrying a `[managed]` seed source (unlocked,
    /// `required = false`).
    fn config_with_seed(source: &str) -> ocx_lib::Config {
        ocx_lib::Config {
            managed: Some(ocx_lib::ManagedConfig {
                source: Some(source.to_string()),
                required: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Builds a system-locked `[managed]` config (declared `required` at the
    /// SYSTEM scope): `system_locked` is sticky and marks the tier non-clearable
    /// / non-redirectable by a lower tier or an explicit flag.
    fn locked_config(source: &str) -> ocx_lib::Config {
        ocx_lib::Config {
            managed: Some(ocx_lib::ManagedConfig {
                source: Some(source.to_string()),
                required: Some(true),
                system_locked: true,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// System-locked + `--managed-config ""` (clear) is refused (exit 78) — a
    /// lock only tightens; clearing would delete the required tier's snapshot.
    #[test]
    fn locked_tier_rejects_explicit_clear() {
        let config = locked_config("system.corp/ocx-config:user");
        let err = resolve_managed_config_arg(Some(""), &config, None).unwrap_err();
        assert!(
            err.to_string().contains("system-locked"),
            "clear against a locked tier must be rejected, got: {err}"
        );
    }

    /// System-locked + a mismatched explicit ref (redirect) is refused (exit 78).
    #[test]
    fn locked_tier_rejects_mismatched_redirect() {
        let config = locked_config("system.corp/ocx-config:user");
        let err = resolve_managed_config_arg(Some("hostile.test/evil-config:latest"), &config, None).unwrap_err();
        assert!(
            err.to_string().contains("system-locked"),
            "redirect against a locked tier must be rejected, got: {err}"
        );
    }

    /// System-locked + an explicit ref matching the locked source proceeds
    /// (re-adopt / self-heal at the same source is fine).
    #[test]
    fn locked_tier_accepts_matching_ref() {
        let config = locked_config("system.corp/ocx-config:user");
        let resolved = resolve_managed_config_arg(Some("system.corp/ocx-config:user"), &config, None)
            .expect("a matching explicit ref must proceed");
        assert_eq!(resolved.as_deref(), Some("system.corp/ocx-config:user"));
    }

    /// An explicit flag — including the empty clear form — passes through
    /// verbatim, never overridden by env or seed.
    #[test]
    fn explicit_flag_passes_through_verbatim() {
        let config = config_with_seed("seed.example.com/ocx-config:user");
        let resolved = resolve_managed_config_arg(
            Some("flag.example.com/ocx-config:user"),
            &config,
            Some("env.example.com/ocx-config:user"),
        )
        .unwrap();
        assert_eq!(resolved.as_deref(), Some("flag.example.com/ocx-config:user"));

        let cleared = resolve_managed_config_arg(Some(""), &config, None).unwrap();
        assert_eq!(cleared.as_deref(), Some(""), "explicit clear must survive");
    }

    /// Flag omitted: the `OCX_MANAGED_CONFIG` env override wins over the seed.
    #[test]
    fn omitted_flag_falls_back_to_env_over_seed() {
        let config = config_with_seed("seed.example.com/ocx-config:user");
        let resolved = resolve_managed_config_arg(None, &config, Some("env.example.com/ocx-config:user")).unwrap();
        assert_eq!(resolved.as_deref(), Some("env.example.com/ocx-config:user"));
    }

    /// Flag omitted, no env: the `[managed].source` seed is adopted (this is
    /// what lets a bare run re-adopt and self-heal).
    #[test]
    fn omitted_flag_falls_back_to_seed() {
        let config = config_with_seed("seed.example.com/ocx-config:user");
        let resolved = resolve_managed_config_arg(None, &config, None).unwrap();
        assert_eq!(resolved.as_deref(), Some("seed.example.com/ocx-config:user"));
    }

    /// Flag omitted, no env, no seed: nothing resolves — `config setup` maps
    /// this to a usage error (64) in `execute`, unlike `self setup`'s no-op.
    #[test]
    fn omitted_flag_with_nothing_configured_is_none() {
        let config = ocx_lib::Config::default();
        let resolved = resolve_managed_config_arg(None, &config, None).unwrap();
        assert_eq!(resolved, None);
    }
}
