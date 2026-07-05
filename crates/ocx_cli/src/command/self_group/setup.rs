// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use ocx_lib::cli::ExitCode as OcxExitCode;
use ocx_lib::env;
use ocx_lib::setup::{self, SetupOptions, SetupOutcome, VersionSpec};

use crate::api::data::self_setup::SelfSetupData;

/// Create or refresh ocx shell integration.
///
/// Completes a bare-binary install: installs the latest published ocx into the
/// content store, writes the per-shell env shims into `$OCX_HOME`, and adds a
/// managed activation block to your shell profiles. Re-running is safe: the
/// shims and blocks are diff-gated, so an unchanged setup is a no-op.
///
/// Pass an optional VERSION to install a specific release instead of the
/// latest. VERSION accepts a tag (`1.2.3`), a digest (`sha256:<hex>`), or both
/// (`1.2.3@sha256:<hex>` — an immutability assertion that fails if the tag
/// resolves to a different digest).
///
/// The managed block is fenced (`# >>> ocx v1 <hash> >>>`). If you edit the
/// block by hand, that profile is reported as dirty and left untouched (exit
/// 82); pass `--force` to overwrite it. Pass `--no-modify-path` (or set
/// `OCX_NO_MODIFY_PATH` to a truthy value) to write the shims without touching
/// any profile (the opt-out is not remembered, so repeat it each run).
///
/// On Windows, a `Restricted` execution policy makes the profile block inert;
/// setup prints how to relax it but never changes the policy itself.
///
/// See https://ocx.sh/docs/user-guide#install-bare-binary for the full setup
/// walkthrough.
///
/// # Exit codes
///
/// | Outcome | Exit |
/// |---|---|
/// | completed / no-op / migrated | 0 |
/// | bad VERSION syntax | 64 |
/// | tag@digest mismatch (immutability assertion failed) | 65 |
/// | registry unreachable | 69 |
/// | writing env shims or profile failed | 74 |
/// | invalid `--managed-config` seed or source | 78 |
/// | package not found in registry | 79 |
/// | authentication failed while fetching the managed-config snapshot | 80 |
/// | bootstrap blocked (offline, not installed) | 81 |
/// | a profile was dirty and skipped (no `--force`) | 82 |
#[derive(Parser)]
pub struct SelfSetup {
    /// Version to install: tag, `sha256:<hex>`, or `tag@sha256:<hex>`.
    ///
    /// Omit to install the latest published release. A tag installs that exact
    /// release. A digest installs the exact content. A `tag@digest` form
    /// verifies the tag resolves to the given digest (immutability assertion).
    ///
    /// The literal `latest` resolves only if the registry publishes such a tag;
    /// omitting VERSION is the recommended way to request the latest release.
    ///
    // NOTE: `require_equals` is NOT needed here — a single-value typed positional
    // plus named repeatable `--profile` is unambiguous to clap without it.
    #[arg(value_name = "VERSION", value_parser = |s: &str| VersionSpec::from_str(s).map_err(|e| e.to_string()))]
    version: Option<VersionSpec>,

    /// Write the env shims but do not modify any shell profile.
    ///
    /// A truthy `OCX_NO_MODIFY_PATH` (`1`/`y`/`yes`/`on`/`true`) sets this too. The
    /// opt-out is not remembered between runs - repeat the flag (or keep the env
    /// var set) each invocation.
    #[arg(long, default_value_t = env::flag(env::keys::OCX_NO_MODIFY_PATH, false))]
    no_modify_path: bool,

    /// Target an explicit profile file. Repeatable. Default: auto-detect.
    ///
    /// Explicit targets are written with POSIX-fence semantics regardless of
    /// the file name.
    #[arg(long, value_name = "PATH")]
    profile: Vec<PathBuf>,

    /// Report the intended actions without writing anything.
    #[arg(long)]
    dry_run: bool,

    /// Overwrite a managed block that carries user edits (the dirty state).
    #[arg(long)]
    force: bool,

    /// Adopt (or clear) the corporate managed-config tier.
    ///
    /// Resolves an OCI reference to a managed-config artifact, synchronously
    /// fetches and persists a snapshot, and only then writes the `[managed]`
    /// seed fence in `$OCX_HOME/config.toml` - a fetch failure leaves no
    /// partial state. Pass an empty string
    /// (`--managed-config ""`) to clear an existing seed and delete the
    /// snapshot.
    ///
    /// Precedence when omitted: `OCX_MANAGED_CONFIG` env var, then the
    /// existing seed. Omit entirely to leave the managed-config tier
    /// untouched.
    #[arg(long, value_name = "REF")]
    managed_config: Option<String>,
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
/// Effect: a bare `ocx self setup` re-run now re-adopts a configured seed, so
/// the lib's Current-fence arm can self-heal a wiped or mismatched snapshot.
///
/// A system-locked tier refuses an explicit value that would clear or redirect
/// it (exit 78) — a lock only tightens, and clearing/refetching would otherwise
/// corrupt the required tier the lock protects.
fn resolve_managed_config_arg(
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

impl SelfSetup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let managed_config = resolve_managed_config_arg(
            self.managed_config.as_deref(),
            context.config(),
            context.managed_config_env_override(),
        )?;
        let options = SetupOptions {
            no_modify_path: self.no_modify_path,
            profiles: self.profile.clone(),
            dry_run: self.dry_run,
            force: self.force,
            version: self.version.clone(),
            managed_config,
        };

        let outcome = setup::run(&options, context.manager(), context.file_structure()).await?;

        // Advisories go to stderr (human diagnostics), never the data stream.
        emit_advisories(&context, &outcome);

        // A dirty profile left untouched (no --force) is a non-error outcome;
        // the exit code is decided here by inspecting the outcomes (contract 4).
        // Dry-run never returns the dirty code — it only reports would-skip.
        let exit = exit_code_for(&outcome, self.force, self.dry_run);

        context.api().report(&SelfSetupData::from_outcome(&outcome))?;
        Ok(exit)
    }
}

/// Print the non-fatal advisories to stderr: the Windows exec-policy hint, a
/// shadowing-`ocx` warning, and the "re-source your profile" reload hint.
fn emit_advisories(context: &crate::app::Context, outcome: &SetupOutcome) {
    if let Some(warning) = &outcome.exec_policy_warning {
        context.ui().warn(warning);
    }
    if let Some(path) = &outcome.conflicting_ocx {
        context.ui().warn(format!(
            "another ocx at {} shadows the one ocx self setup just installed",
            path.display()
        ));
    }
    if outcome.reload_hint {
        context.ui().status(
            "Setup",
            "re-source your shell profile (or open a new shell) to activate ocx",
        );
    }
}

/// Decide the process exit code from the run outcome.
///
/// A profile left untouched because the user edited it (no `--force`) maps to
/// [`OcxExitCode::DirtyRcBlock`] (82) so a script can detect it. `--force`
/// rewrites the block (so no profile is `SkippedDirty`) and `dry_run` only
/// reports would-skip — neither returns 82. The `[managed]` fence carries the
/// same dirty-fence contract (criterion 5) via
/// [`ocx_lib::setup::ManagedConfigSetupOutcome::Dirty`].
fn exit_code_for(outcome: &SetupOutcome, force: bool, dry_run: bool) -> ExitCode {
    let profile_dirty = setup::profiles_dirty(&outcome.profiles);
    let managed_config_dirty = matches!(outcome.managed_config, ocx_lib::setup::ManagedConfigSetupOutcome::Dirty);
    if (profile_dirty || managed_config_dirty) && !force && !dry_run {
        return OcxExitCode::DirtyRcBlock.into();
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ocx_lib::setup::{BootstrapOutcome, BootstrapStatus, ManagedConfigSetupOutcome, ProfileOutcome, SetupOutcome};

    use super::{exit_code_for, resolve_managed_config_arg};

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
    /// what lets a bare `ocx self setup` re-adopt and self-heal).
    #[test]
    fn omitted_flag_falls_back_to_seed() {
        let config = config_with_seed("seed.example.com/ocx-config:user");
        let resolved = resolve_managed_config_arg(None, &config, None).unwrap();
        assert_eq!(resolved.as_deref(), Some("seed.example.com/ocx-config:user"));
    }

    /// Flag omitted, no env, no seed: nothing resolves — the tier is left
    /// untouched (`None` → lib reports `NotConfigured`).
    #[test]
    fn omitted_flag_with_nothing_configured_is_none() {
        let config = ocx_lib::Config::default();
        let resolved = resolve_managed_config_arg(None, &config, None).unwrap();
        assert_eq!(resolved, None);
    }

    fn outcome(profiles: Vec<(PathBuf, ProfileOutcome)>) -> SetupOutcome {
        SetupOutcome {
            bootstrap: BootstrapOutcome {
                status: BootstrapStatus::AlreadyPresent,
                version: None,
                digest: None,
            },
            shims_written: Vec::new(),
            profiles,
            exec_policy_warning: None,
            conflicting_ocx: None,
            reload_hint: false,
            managed_config: ManagedConfigSetupOutcome::NotConfigured,
        }
    }

    /// Round-trip an `ExitCode` through its Debug form to compare against a
    /// known numeric value (`ExitCode` is opaque, but `From<u8>` is stable).
    fn exit_code_equals(actual: std::process::ExitCode, expected: u8) -> bool {
        format!("{actual:?}") == format!("{:?}", std::process::ExitCode::from(expected))
    }

    /// A dirty profile without `--force` maps to exit 82.
    #[test]
    fn dirty_without_force_is_exit_82() {
        let base = outcome(vec![(PathBuf::from(".zshrc"), ProfileOutcome::SkippedDirty)]);
        assert!(exit_code_equals(exit_code_for(&base, false, false), 82));
    }

    /// `--force` rewrites the block (no SkippedDirty in the outcome), so it is
    /// exit 0 — but even a stray SkippedDirty under force stays 0.
    #[test]
    fn dirty_with_force_is_success() {
        let base = outcome(vec![(PathBuf::from(".zshrc"), ProfileOutcome::SkippedDirty)]);
        assert!(exit_code_equals(exit_code_for(&base, true, false), 0));
    }

    /// Dry-run never returns 82 even when a profile would be skipped dirty.
    #[test]
    fn dirty_dry_run_is_success() {
        let base = outcome(vec![(PathBuf::from(".zshrc"), ProfileOutcome::SkippedDirty)]);
        assert!(exit_code_equals(exit_code_for(&base, false, true), 0));
    }

    /// A clean run (completed / no-op profiles) is exit 0.
    #[test]
    fn clean_run_is_success() {
        let base = outcome(vec![
            (PathBuf::from(".bashrc"), ProfileOutcome::Completed),
            (PathBuf::from(".zshrc"), ProfileOutcome::NoOp),
        ]);
        assert!(exit_code_equals(exit_code_for(&base, false, false), 0));
    }
}
