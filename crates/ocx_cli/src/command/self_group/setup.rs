// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use ocx_lib::cli::ExitCode as OcxExitCode;
use ocx_lib::env;
use ocx_lib::setup::shell_config::{self, ShellFlag};
use ocx_lib::setup::{self, SetupOptions, SetupOutcome, VersionSpec};
use ocx_lib::{ConfigTier, ShellConfig};

// The `--managed-config` precedence seam (`resolve_managed_config_arg`) is
// shared with `ocx config setup` and lives in `command/config_setup.rs`.
use crate::api::data::self_setup::SelfSetupData;
use crate::command::config_setup::resolve_managed_config_arg;

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
/// Pass `--hook` / `--no-hook` or `--completion` / `--no-completion` to record
/// that preference as `[shell] hook` / `[shell] completions` in
/// `$OCX_HOME/config.toml`. The edit sets exactly that one key and leaves the
/// rest of the file — comments included — untouched; it is not a managed
/// block, so it is never reported dirty. Omit a pair to leave the file alone.
/// A tier above `$OCX_HOME` (a managed config, or `--config` / `OCX_CONFIG`)
/// still wins, and setup says so.
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
/// | managed config adopted / refreshed / already adopted / cleared | 0 |
/// | managed-config refresh of an already-adopted seed failed (snapshot kept) | 0 |
/// | bad VERSION syntax | 64 |
/// | tag@digest mismatch (immutability assertion failed) | 65 |
/// | registry unreachable | 69 |
/// | writing env shims, a profile, or the `[shell]` toggle failed | 74 |
/// | invalid `--managed-config` seed or source | 78 |
/// | package not found in registry | 79 |
/// | authentication failed while fetching the managed-config snapshot | 80 |
/// | bootstrap blocked (offline, not installed) | 81 |
/// | a profile was dirty and skipped (no `--force`) | 82 |
///
/// The registry codes (69 / 79 / 80) apply to the managed-config tier only
/// when it is being adopted for the first time, or when the snapshot on disk
/// is missing or belongs to another source. Once a matching snapshot exists,
/// a failed refresh *fetch* keeps it and reports `refresh_unavailable` with
/// exit 0; a failure writing the refreshed snapshot to disk still errors (74).
#[derive(Parser)]
pub struct SelfSetup {
    /// Turn the per-prompt shell hook on: writes `[shell] hook = true`.
    ///
    /// Omit both this and `--no-hook` to leave `config.toml` untouched.
    #[arg(long = "hook", overrides_with = "no_hook")]
    hook: bool,

    /// Turn the per-prompt shell hook off: writes `[shell] hook = false`.
    #[arg(long = "no-hook", overrides_with = "hook")]
    no_hook: bool,

    /// Turn shell completions on: writes `[shell] completions = true`.
    ///
    /// Omit both this and `--no-completion` to leave `config.toml` untouched.
    #[arg(long = "completion", overrides_with = "no_completion")]
    completion: bool,

    /// Turn shell completions off: writes `[shell] completions = false`.
    #[arg(long = "no-completion", overrides_with = "completion")]
    no_completion: bool,

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
    ///
    /// Every run reconciles an already-adopted seed against the registry, so a
    /// newer published config is picked up here too. If that refresh cannot
    /// reach the registry, the existing snapshot is kept and setup still
    /// succeeds.
    #[arg(long, value_name = "REF")]
    managed_config: Option<String>,
}

impl SelfSetup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Before the bootstrap, not after: the `[shell]` write is a local,
        // deterministic edit that does not depend on the install succeeding,
        // and running it first means a registry failure cannot silently drop
        // the toggle the user asked for.
        self.apply_shell_flags(&context)?;

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

        let outcome = setup::run(&options, context.config(), context.manager(), context.file_structure()).await?;

        // Advisories go to stderr (human diagnostics), never the data stream.
        emit_advisories(&context, &outcome);

        // A dirty profile left untouched (no --force) is a non-error outcome;
        // the exit code is decided here by inspecting the outcomes (contract 4).
        // Dry-run never returns the dirty code — it only reports would-skip.
        let exit = exit_code_for(&outcome, self.force, self.dry_run);

        context.api().report(&SelfSetupData::from_outcome(&outcome))?;
        Ok(exit)
    }

    /// Write the `[shell]` toggles this invocation asked for into the home tier
    /// (C-040), and say which tier will still decide when a higher one already
    /// sets the key (C-034).
    ///
    /// The target is `$OCX_HOME/config.toml` — `--config` / `OCX_CONFIG` name a
    /// **read** override and never redirect this write. The write is not
    /// fenced, so a failure is 74 `IoError`, never 82 `DirtyRcBlock`.
    fn apply_shell_flags(&self, context: &crate::app::Context) -> anyhow::Result<()> {
        let writes = shell_writes(self);
        if writes.is_empty() {
            return Ok(());
        }
        let config_path = context.file_structure().root().join("config.toml");

        for (flag, value) in writes {
            if self.dry_run {
                context.ui().status(
                    "Setup",
                    format!(
                        "would set [shell] {key} = {value} in {path}",
                        key = flag.key(),
                        path = config_path.display()
                    ),
                );
            } else {
                shell_config::set_flag(&config_path, flag, value)?;
            }

            // Above the dry-run guard on purpose: which tier decides is a
            // property of the setting, not of the byte-write, and `--dry-run`
            // is the mode a user runs specifically to find out whether the
            // toggle will take effect. The context's config was merged before
            // this write, so it still names whichever tier set the key going
            // in — exactly the tier that keeps deciding once the home tier
            // says otherwise.
            if let Some(tier) = overriding_tier(context.config().shell.as_ref(), flag) {
                context.ui().warn(format!(
                    "[shell] {key} is also set by {tier}, which wins over {path} - the value {written} will not take effect",
                    key = flag.key(),
                    path = config_path.display(),
                    written = if self.dry_run { "this would write" } else { "just written" },
                ));
            }
        }
        Ok(())
    }
}

/// The `[shell]` writes this invocation asked for, in `hook`-then-`completions`
/// order (C-040).
///
/// **A pair with neither flag contributes nothing** — that is what makes
/// `ocx self setup` with no new flag leave `config.toml` byte-identical.
fn shell_writes(setup: &SelfSetup) -> Vec<(ShellFlag, bool)> {
    [
        (ShellFlag::Hook, requested(setup.hook, setup.no_hook)),
        (ShellFlag::Completions, requested(setup.completion, setup.no_completion)),
    ]
    .into_iter()
    .filter_map(|(flag, value)| Some((flag, value?)))
    .collect()
}

/// Collapse one `--X` / `--no-X` pair into the value it requests, or `None`
/// when neither flag was given.
///
/// `overrides_with` already makes clap last-wins, so both-set is unreachable
/// from a command line; the off-wins tie-break is pinned anyway because the
/// struct is constructible, and it matches `options::Hook`'s.
fn requested(on: bool, off: bool) -> Option<bool> {
    match (on, off) {
        (_, true) => Some(false),
        (true, false) => Some(true),
        (false, false) => None,
    }
}

/// The tier that will still decide `flag` after the home-tier write lands, or
/// `None` when the write itself decides (C-034 / A-32).
fn overriding_tier(shell: Option<&ShellConfig>, flag: ShellFlag) -> Option<ConfigTier> {
    let shell = shell?;
    let tier = match flag {
        ShellFlag::Hook => shell.hook_tier,
        ShellFlag::Completions => shell.completions_tier,
    }?;
    // `ConfigTier` is ordered System < User < Home < Managed < Explicit, which
    // is also the fold order, so "still decides after a home-tier write" is
    // exactly "ranks above Home". The tier is reported by name (A-32) rather
    // than assumed to be the managed one.
    (tier > ConfigTier::Home).then_some(tier)
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

    use clap::Parser as _;
    use ocx_lib::setup::{BootstrapOutcome, BootstrapStatus, ManagedConfigSetupOutcome, ProfileOutcome, SetupOutcome};

    use super::*;

    fn parse(args: &[&str]) -> SelfSetup {
        SelfSetup::try_parse_from(std::iter::once("setup").chain(args.iter().copied())).expect("valid grammar")
    }

    fn stamped(flag: ShellFlag, tier: ConfigTier) -> ShellConfig {
        let mut shell = ShellConfig::default();
        match flag {
            ShellFlag::Hook => {
                shell.hook = Some(true);
                shell.hook_tier = Some(tier);
            }
            ShellFlag::Completions => {
                shell.completions = Some(true);
                shell.completions_tier = Some(tier);
            }
        }
        shell
    }

    /// C-040 / S-016 — the load-bearing negative: `ocx self setup` with neither
    /// flag of a pair requests no write at all, so `config.toml` is left
    /// byte-identical.
    #[test]
    fn neither_flag_requests_no_write() {
        assert!(shell_writes(&parse(&[])).is_empty());
        assert!(
            shell_writes(&parse(&["1.2.3", "--dry-run"])).is_empty(),
            "an unrelated flag or the positional must not conjure a [shell] write"
        );
    }

    /// C-040: each pair writes its own key, in both directions.
    #[test]
    fn each_pair_writes_its_own_key_in_both_directions() {
        for (args, expected) in [
            (vec!["--hook"], vec![(ShellFlag::Hook, true)]),
            (vec!["--no-hook"], vec![(ShellFlag::Hook, false)]),
            (vec!["--completion"], vec![(ShellFlag::Completions, true)]),
            (vec!["--no-completion"], vec![(ShellFlag::Completions, false)]),
            (
                vec!["--no-hook", "--completion"],
                vec![(ShellFlag::Hook, false), (ShellFlag::Completions, true)],
            ),
        ] {
            assert_eq!(shell_writes(&parse(&args)), expected, "for {args:?}");
        }
    }

    /// C-040: the pairs are POSIX last-wins, so passing both is not an error —
    /// `overrides_with` clears the loser, and the survivor decides.
    #[test]
    fn a_repeated_pair_is_last_wins_not_an_error() {
        assert_eq!(
            shell_writes(&parse(&["--hook", "--no-hook"])),
            vec![(ShellFlag::Hook, false)]
        );
        assert_eq!(
            shell_writes(&parse(&["--no-hook", "--hook"])),
            vec![(ShellFlag::Hook, true)]
        );
        assert_eq!(
            shell_writes(&parse(&["--completion", "--no-completion"])),
            vec![(ShellFlag::Completions, false)]
        );
        assert_eq!(
            shell_writes(&parse(&["--no-completion", "--completion"])),
            vec![(ShellFlag::Completions, true)]
        );
    }

    /// C-040: the flags sit before the positional and are booleans, so VERSION
    /// is never swallowed by one of them.
    #[test]
    fn the_positional_survives_a_preceding_flag() {
        let parsed = parse(&["--hook", "1.2.3"]);
        assert_eq!(
            parsed.version.as_ref().map(ToString::to_string),
            Some("1.2.3".to_owned())
        );
        assert_eq!(shell_writes(&parsed), vec![(ShellFlag::Hook, true)]);
    }

    /// C-034 / S-016(b): a tier above home still decides after the write, and
    /// it is named by the tier that actually set the key — never a hard-coded
    /// "managed".
    #[test]
    fn a_higher_tier_is_reported_by_name() {
        for tier in [ConfigTier::Managed, ConfigTier::Explicit] {
            assert_eq!(
                overriding_tier(Some(&stamped(ShellFlag::Hook, tier)), ShellFlag::Hook),
                Some(tier)
            );
            assert_eq!(
                overriding_tier(Some(&stamped(ShellFlag::Completions, tier)), ShellFlag::Completions),
                Some(tier)
            );
        }
    }

    /// C-034: a tier at or below home loses to the home-tier write, so there is
    /// nothing to report — and neither does a key no tier set.
    #[test]
    fn home_and_below_are_not_reported() {
        for tier in [ConfigTier::System, ConfigTier::User, ConfigTier::Home] {
            assert_eq!(
                overriding_tier(Some(&stamped(ShellFlag::Hook, tier)), ShellFlag::Hook),
                None
            );
        }
        assert_eq!(overriding_tier(None, ShellFlag::Hook), None);
        assert_eq!(
            overriding_tier(Some(&ShellConfig::default()), ShellFlag::Hook),
            None,
            "an unset key has no deciding tier"
        );
    }

    /// C-040 drift guard: the four long flags `self setup` declares are the
    /// same four `options::Hook` / `options::Completion` declare.
    ///
    /// `self setup` re-declares them instead of flattening the shared types,
    /// because it only **records** a preference and never resolves the ladder —
    /// `Hook::enabled` wants an interactivity signal and a `configured` value
    /// this command has neither of. The cost of that is a second declaration
    /// that can drift, so both sides are read back out of clap rather than
    /// spelled out here: rename a flag on either side, or add a fifth to the
    /// shared types, and this reds.
    #[test]
    fn the_shell_toggles_declare_the_shared_flag_names() {
        use std::collections::BTreeSet;

        use clap::{Args as _, Command, CommandFactory as _};

        fn long_flags(command: &Command) -> BTreeSet<&str> {
            command.get_arguments().filter_map(clap::Arg::get_long).collect()
        }

        let shared =
            crate::options::Completion::augment_args(crate::options::hook::Hook::augment_args(Command::new("shared")));
        let ours = SelfSetup::command();

        let shared_flags = long_flags(&shared);
        assert_eq!(shared_flags.len(), 4, "the shared types declare two pairs");
        assert!(
            shared_flags.is_subset(&long_flags(&ours)),
            "`self setup` must declare every `[shell]` toggle the shared option types do; \
             shared = {shared_flags:?}"
        );
    }

    /// C-040 drift guard, second half: `requested`'s tie-break is a hand copy
    /// of the one `options::Hook` uses for rungs 1 and 2, so it is compared
    /// against the original rather than trusted to have stayed equal.
    #[test]
    fn the_flag_tie_break_matches_the_shared_ladder() {
        use clap::Parser as _;

        use crate::options::hook::Rung;

        #[derive(clap::Parser)]
        struct Shared {
            #[clap(flatten)]
            hook: crate::options::hook::Hook,
        }

        for args in [
            vec![],
            vec!["--hook"],
            vec!["--no-hook"],
            vec!["--hook", "--no-hook"],
            vec!["--no-hook", "--hook"],
        ] {
            let shared = Shared::try_parse_from(std::iter::once("x").chain(args.iter().copied()))
                .expect("valid grammar")
                .hook;
            // Rungs 1 and 2 are the flag rungs; anything below them means the
            // flag was absent. `configured: None` keeps rung 4 out of the way.
            let shared_flag = match shared.rung(false, None) {
                Rung::FlagOff => Some(false),
                Rung::FlagOn => Some(true),
                _ => None,
            };
            assert_eq!(
                shell_writes(&parse(&args)).first().map(|(_, value)| *value),
                shared_flag,
                "for {args:?}"
            );
        }
    }

    /// C-034: the provenance is per key — a managed `hook` says nothing about
    /// who decides `completions`.
    #[test]
    fn the_report_is_per_key() {
        let shell = stamped(ShellFlag::Hook, ConfigTier::Managed);
        assert_eq!(
            overriding_tier(Some(&shell), ShellFlag::Hook),
            Some(ConfigTier::Managed)
        );
        assert_eq!(overriding_tier(Some(&shell), ShellFlag::Completions), None);
    }

    // The `resolve_managed_config_arg` precedence tests live with the shared
    // seam in `command/config_setup.rs`.

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
