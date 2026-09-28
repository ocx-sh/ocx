// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx config setup`: config-only managed-config adoption.
//!
//! Shares `ocx_setup::apply_managed_config` with `ocx self setup --managed-config`, so precedence,
//! fetch-first ordering and the dirty-fence contract match by construction.

use std::process::ExitCode;

use clap::Parser;
use ocx_exit::ExitCode as OcxExitCode;

use crate::api::data::config_setup::ConfigSetupData;

/// Adopt (or clear) the corporate managed-config tier.
///
/// Resolves the source (flag, then `OCX_MANAGED_CONFIG`, then the existing
/// seed), fetches and persists a snapshot, and only then writes the
/// `[managed]` seed fence, so a fetch failure leaves no partial state. Every
/// run re-syncs an already-adopted seed; an unreachable registry keeps the
/// snapshot and still succeeds. Nothing resolved is a usage error: unlike
/// `ocx self setup`, this command exists only to set up the tier.
///
/// Exit codes: <https://ocx.sh/docs/reference/command-line#config-setup>
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

/// Resolve the effective `--managed-config` value: `Some(ref)` adopts, `Some("")` clears, `None`
/// leaves the tier untouched. An explicit flag passes through; an omitted one falls back to
/// [`ocx_config::managed::resolve_managed_target`]. Shared by `config setup` and `self setup`.
///
/// # Errors
///
/// A system-locked tier refuses an explicit value that would clear or redirect it (78).
pub fn resolve_managed_config_arg(
    flag: Option<&str>,
    config: &ocx_config::Config,
    env_override: Option<&str>,
) -> anyhow::Result<Option<String>> {
    if let Some(value) = flag {
        ocx_config::managed::check_locked_managed_override(config, value)?;
        return Ok(Some(value.to_string()));
    }
    Ok(ocx_config::managed::resolve_managed_target(config, env_override)?.map(|resolved| resolved.source.to_string()))
}

impl ConfigSetupArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let resolved = resolve_managed_config_arg(
            self.managed_config.as_deref(),
            context.config(),
            context.managed_config_env_override(),
        )?;

        // Unlike `self setup`, which has other phases, nothing to set up here is a usage error.
        let Some(value) = resolved else {
            return Err(crate::error::UsageError::new(
                "nothing to set up: pass --managed-config <REF>, set OCX_MANAGED_CONFIG, \
                 or configure a [managed] seed",
            )
            .into());
        };

        let outcome = ocx_setup::apply_managed_config(
            context.config(),
            Some(&value),
            self.dry_run,
            self.force,
            context.manager(),
            context.file_structure(),
        )
        .await?;

        // Exit 82 on an untouched dirty fence, as `self setup`; dry-run never returns it.
        let dirty = matches!(outcome, ocx_setup::ManagedConfigSetupOutcome::Dirty);
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
    fn config_with_seed(source: &str) -> ocx_config::Config {
        ocx_config::Config {
            managed: Some(ocx_config::managed::ManagedConfig {
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
    fn locked_config(source: &str) -> ocx_config::Config {
        ocx_config::Config {
            managed: Some(ocx_config::managed::ManagedConfig {
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
        let config = ocx_config::Config::default();
        let resolved = resolve_managed_config_arg(None, &config, None).unwrap();
        assert_eq!(resolved, None);
    }
}
