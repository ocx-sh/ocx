// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use ocx_lib::activate::ActivateMode;
use ocx_lib::cli::ExitCode as OcxExitCode;
use ocx_lib::env;
use ocx_lib::setup::shell_config::{self, ShellFlag};
use ocx_lib::setup::{self, SessionPathOutcome, SetupOptions, SetupOutcome, VersionSpec};
use ocx_lib::{ConfigTier, ShellConfig};

// The `--managed-config` precedence seam (`resolve_managed_config_arg`) is
// shared with `ocx config setup` and lives in `command/config_setup.rs`.
use crate::api::data::self_setup::SelfSetupData;
use crate::command::config_setup::resolve_managed_config_arg;

/// Arguments of `ocx self setup`.
///
/// **Not the rendered help.** clap takes a subcommand's `about`/`long_about`
/// from the *variant* that holds this struct — `SelfGroup::Setup` — so the
/// user-facing description of what setup does lives there, and this comment is
/// maintainer documentation only. Each field below still renders as its own
/// argument help.
///
/// The managed profile block is fenced (`# >>> ocx v1 <hash> >>>`); an edited
/// fence is reported dirty and left alone (exit 82) unless `--force` is passed.
/// A `[shell]` toggle write sets exactly one key and leaves the rest of
/// `config.toml` — comments included — untouched; it is not a managed block, so
/// it is never reported dirty, and a tier above `$OCX_HOME` still wins. On
/// Windows, a `Restricted` execution policy makes the profile block inert;
/// setup prints how to relax it but never changes the policy itself.
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
/// | writing env shims, a profile, the `[shell]` toggle, or the `activate` key failed | 74 |
/// | invalid `--managed-config` seed or source | 78 |
/// | `$OCX_HOME` cannot be encoded for this platform's session-PATH format | 78 |
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

    /// Record how a toolchain reaches PATH: writes `activate` to
    /// `$OCX_HOME/ocx.toml`.
    ///
    /// `env` composes the toolchain environment on every prompt. `bin` puts
    /// the toolchain's `bin` directory on PATH and composes nothing else, so
    /// each tool is resolved by its launcher when it runs. `none` composes
    /// nothing and adds nothing.
    ///
    /// For the global toolchain this flag writes, `bin` and `none` leave the
    /// same PATH: `$OCX_HOME/toolchain/active/bin` is a session directory `ocx
    /// self setup` registers once and no prompt withdraws, so the global tools stay
    /// reachable through their trampolines under either. The two values part
    /// company only for a project's own toolchain.
    ///
    /// Omit to leave `ocx.toml` untouched; the file is created carrying only
    /// this key if it does not exist yet. Writes the global toolchain's
    /// setting - a project's own `ocx.toml` still decides for that project,
    /// and `OCX_TOOLCHAIN_ACTIVATE` is the weakest tier of all, consulted only
    /// when no file sets the key.
    #[arg(long = "toolchain-activate", value_enum, value_name = "MODE")]
    toolchain_activate: Option<ActivateMode>,

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

    /// Write the env shims but touch neither a shell profile nor the session
    /// PATH.
    ///
    /// Suppresses both PATH surfaces: the managed activation block in your
    /// shell profiles, and the session-level registration (the Windows user
    /// environment, an `environment.d` drop-in on Linux, a login LaunchAgent
    /// on macOS). The run reports each location it did not touch.
    ///
    /// A truthy `OCX_NO_MODIFY_PATH` (`1`/`y`/`yes`/`on`/`true`) sets this too. The
    /// opt-out is not remembered between runs - repeat the flag (or keep the env
    /// var set) each invocation.
    // `env::flag` treats an unrecognised value - including the empty string -
    // as a WARN plus the default, so `OCX_NO_MODIFY_PATH=""` runs the arm. That
    // is deliberately NOT the "an empty string is absent" rule the toolchain
    // tiers follow (RUL-24): this is a pre-existing boolean with its own
    // semantics, and unifying the two would change a shipped answer for no
    // gain. Stated here because a reader who knows the toolchain rule would
    // otherwise assume one rule covers both.
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
        self.apply_toolchain_activate(&context).await?;

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
        emit_advisories(&context, &outcome, self.dry_run);

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

    /// Write the `activate` mode this invocation asked for into the **global**
    /// `$OCX_HOME/ocx.toml`.
    ///
    /// The sibling of [`Self::apply_shell_flags`], and it keeps that method's
    /// two rules: an omitted flag writes nothing at all, so a re-run leaves
    /// the file byte-identical; and `--dry-run` reports the write it would
    /// make rather than making it.
    ///
    /// The target is the ocx home's own `ocx.toml`, never the project in
    /// effect — `--project` / `OCX_PROJECT` name a different toolchain and
    /// this flag does not redirect onto it. The write itself belongs to
    /// [`ocx_lib::project::mutate`], which owns the file's lock and its
    /// format-preserving edit.
    async fn apply_toolchain_activate(&self, context: &crate::app::Context) -> anyhow::Result<()> {
        let Some(mode) = self.toolchain_activate else {
            return Ok(());
        };
        let config_path = context.file_structure().root().join("ocx.toml");

        if self.dry_run {
            context.ui().status(
                "Setup",
                format!("would set activate = {mode} in {path}", path = config_path.display()),
            );
            return Ok(());
        }
        ocx_lib::project::set_activate(&config_path, mode).await?;
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

/// Print the non-fatal advisories to stderr: the Windows exec-policy hint, the
/// `--dry-run` session-PATH preview, a shadowing-`ocx` warning, a session-PATH
/// store that could not be written, and one reload hint per PATH surface this
/// run changed.
///
/// `dry_run` is the flag this command already holds, not a new outcome variant:
/// [`SessionPathOutcome`] answers "what is the state of this store", and a dry
/// run's own distinguishing evidence is that not one byte moved. Without this
/// preview the run summary reports a store as `written` on a run that wrote
/// nothing, which reads as a completed write.
fn emit_advisories(context: &crate::app::Context, outcome: &SetupOutcome, dry_run: bool) {
    if let Some(warning) = &outcome.exec_policy_warning {
        context.ui().warn(warning);
    }
    if dry_run {
        let directories = setup::session_path_directories(context.file_structure());
        for (location, _) in outcome
            .session_path
            .iter()
            .filter(|(_, session_outcome)| *session_outcome == SessionPathOutcome::Written)
        {
            for directory in &directories {
                // `{:?}`, not `Display`: a store path is derived from
                // `$XDG_CONFIG_HOME`/`$HOME` and a directory from `$OCX_HOME`,
                // none of them validated for line breaks, and a raw newline
                // here would forge a second advisory line (CWE-117). The
                // `SessionPathError` messages quote their paths for the same
                // reason.
                context
                    .ui()
                    .status("Setup", format!("would register {directory:?} in {location:?}"));
            }
        }
    }
    // C-036: a session-PATH write failure is an outcome, not an error — exit 0.
    // The warning is what keeps it from being an outcome computed and
    // discarded, and it names the re-run because that is the whole remedy.
    for (location, _) in outcome
        .session_path
        .iter()
        .filter(|(_, session_outcome)| *session_outcome == SessionPathOutcome::Failed)
    {
        context.ui().warn(format!(
            "could not register the ocx directories on the session PATH at {location:?}; \
             shells still work, and re-running `ocx self setup` retries"
        ));
    }
    if let Some(path) = &outcome.conflicting_ocx {
        context.ui().warn(format!(
            "another ocx at {} shadows the one ocx self setup just installed",
            path.display()
        ));
    }
    // One line per surface this run actually changed, because the remedy
    // differs per surface and a single sentence has to be wrong for one of
    // them. A shell profile is re-read by sourcing it; a session PATH is read
    // once when the session starts (the systemd user manager for
    // `environment.d`, `launchd` at login for the macOS agent, the desktop
    // shell for the Windows registry value), so nothing short of a new login
    // picks it up. `outcome.reload_hint` is the disjunction of the same three
    // predicates and drives the JSON field; it is not re-tested here, which
    // would only assert that a disjunct implies its own disjunction.
    if !outcome.shims_written.is_empty() || setup::profiles_changed(&outcome.profiles) {
        context.ui().status(
            "Setup",
            "re-source your shell profile (or open a new shell) to activate ocx",
        );
    }
    if setup::session_path_written(&outcome.session_path) {
        context.ui().status(
            "Setup",
            "log out and back in for the session PATH to reach programs started outside a shell",
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
            session_path: Vec::new(),
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

    // ── C-042 / C-043: `--toolchain-activate` and the PATH opt-out ───────

    /// C-042 / E-C5 — the load-bearing negative, in the same shape as
    /// `neither_flag_requests_no_write`: an omitted `--toolchain-activate`
    /// writes nothing at all, so a run that does not ask for the key leaves
    /// `$OCX_HOME/ocx.toml` byte-identical — and, when absent, does not create
    /// it.
    #[test]
    fn an_omitted_toolchain_activate_requests_no_write() {
        assert!(parse(&[]).toolchain_activate.is_none());
        assert!(
            parse(&["1.2.3", "--dry-run", "--hook"]).toolchain_activate.is_none(),
            "an unrelated flag or the positional must not conjure an `activate` write"
        );
    }

    /// C-042 / C-012 / E-C8: the flag accepts exactly the three wire spellings
    /// `ocx.toml` itself carries, so one vocabulary serves the file and the
    /// command line.
    #[test]
    fn every_activate_mode_parses_from_its_wire_spelling() {
        for (spelling, expected) in [
            ("env", ActivateMode::Env),
            ("bin", ActivateMode::Bin),
            ("none", ActivateMode::None),
        ] {
            assert_eq!(
                parse(&["--toolchain-activate", spelling]).toolchain_activate,
                Some(expected),
                "for {spelling:?}"
            );
        }
    }

    /// C-042 / E-C7: an unknown mode is a clap usage error — exit 64 — rather
    /// than a value that reaches the writer. Nothing is written and nothing is
    /// created, because the parse never completes.
    #[test]
    fn an_unknown_activate_mode_is_a_usage_error() {
        for spelling in ["garbage", "Env", "ENV", ""] {
            let error = SelfSetup::try_parse_from(["setup", "--toolchain-activate", spelling])
                .err()
                .expect("an unknown mode must not parse");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::InvalidValue,
                "for {spelling:?}: {error}"
            );
        }
    }

    /// C-042: the flag takes a value, so the VERSION positional is never
    /// swallowed by it — the same rule `the_positional_survives_a_preceding_flag`
    /// pins for the boolean pairs.
    #[test]
    fn the_positional_survives_the_toolchain_activate_flag() {
        let parsed = parse(&["--toolchain-activate", "bin", "1.2.3"]);
        assert_eq!(
            parsed.version.as_ref().map(ToString::to_string),
            Some("1.2.3".to_owned())
        );
        assert_eq!(parsed.toolchain_activate, Some(ActivateMode::Bin));
    }

    /// D-3: the flag's help states a **promise about behaviour**, not just about
    /// a file write, and the promise is now kept on both tiers — so the text is
    /// pinned and the next person to change the behaviour has to change the
    /// promise too.
    ///
    /// Every clause is load-bearing: the global toolchain reads its own
    /// `activate` (`ocx_cli::command::self_group::activate`'s
    /// `global_prompt_entries`), a project's own `ocx.toml` still decides for
    /// that project (`ocx_lib::activation::project_contribution`), and
    /// `OCX_TOOLCHAIN_ACTIVATE` is the weakest tier on both
    /// (`ocx_lib::activation::activate_mode`). `quality-cli-help.md` rates an
    /// incorrect statement of behaviour in clap-rendered text Block-tier.
    ///
    /// The second literal is C-059's consequence, pinned in the help closest to
    /// the flag rather than only in `command-line.md` / `configuration.md` /
    /// the user guide: `session_path_holds_both_global_directories` keeps
    /// `$OCX_HOME/toolchain/active/bin` desired in *every* mode, so at the one tier
    /// this flag writes, `bin` and `none` reach an identical `PATH`. Help text
    /// that said `none` "does neither" was false for that tier.
    #[test]
    fn the_toolchain_activate_help_pins_the_promise_it_makes_about_both_tiers() {
        let command = <SelfSetup as clap::CommandFactory>::command();
        let long_help = command
            .get_arguments()
            .find(|argument| argument.get_long() == Some("toolchain-activate"))
            .and_then(|argument| argument.get_long_help().map(ToString::to_string))
            .expect("the flag carries long help");
        // clap re-wraps the doc comment, so compare on collapsed whitespace
        // rather than on the source's line breaks.
        let rendered = long_help.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(
            rendered.contains(
                "Writes the global toolchain's setting - a project's own `ocx.toml` still decides \
                 for that project, and `OCX_TOOLCHAIN_ACTIVATE` is the weakest tier of all, \
                 consulted only when no file sets the key."
            ),
            "the promise must survive verbatim; changing the behaviour means changing this \
             sentence in the same commit:\n{rendered}"
        );
        assert!(
            rendered.contains(
                "For the global toolchain this flag writes, `bin` and `none` leave the same PATH: \
                 `$OCX_HOME/toolchain/active/bin` is a session directory `ocx self setup` \
                 registers once and no prompt withdraws, so the global tools stay reachable \
                 through their \
                 trampolines under either. The two values part company only for a project's own \
                 toolchain."
            ),
            "C-059's equivalence must be stated in the help closest to the flag, not only in the \
             website reference; `none` that claimed to compose nothing AND add nothing was false \
             for the one tier this flag writes:\n{rendered}"
        );
    }

    /// C-043 / E-X1: the opt-out is **asymmetric by construction**, and this
    /// pins that as the answer rather than leaving the precedence question
    /// open.
    ///
    /// `OCX_NO_MODIFY_PATH` supplies the flag's `default_value_t`, so the env
    /// var can only set the default and the flag can only turn it **on**. There
    /// is no `--modify-path`, so there is no command-line way to override a
    /// truthy env var — the opt-out fails safe, in the direction that touches
    /// less of the user's machine.
    #[test]
    fn there_is_no_command_line_way_to_turn_the_path_opt_out_back_on() {
        let command = <SelfSetup as clap::CommandFactory>::command();
        let longs: Vec<String> = command
            .get_arguments()
            .filter_map(|argument| argument.get_long().map(str::to_owned))
            .collect();

        assert!(
            longs.iter().any(|long| long == "no-modify-path"),
            "the opt-out must exist: {longs:?}"
        );
        assert!(
            !longs.iter().any(|long| long == "modify-path"),
            "adding `--modify-path` would change C-043's precedence answer: {longs:?}"
        );
    }

    /// C-043 + A-14: `--no-modify-path` now suppresses the **whole
    /// session-PATH arm**, not only the profile blocks.
    ///
    /// `quality-cli-help.md` rates an incorrect statement of behaviour in
    /// clap-rendered text **Block-tier**, and both surfaces below are rendered:
    /// the struct doc becomes `long_about`, the arg doc becomes the flag's
    /// help. The exact wording is the Implement phase's to choose — what is
    /// asserted is that the user-visible text stops describing the flag as
    /// profile-only.
    ///
    /// Note the third surface this cannot reach: clap renders the `Setup`
    /// variant doc in `command/self_group.rs` as the subcommand `about`, and it
    /// carries the same profile-only claim. That file needs the same edit.
    #[test]
    fn the_opt_out_help_text_names_the_session_path_it_now_suppresses() {
        let command = <SelfSetup as clap::CommandFactory>::command();

        let long_about = command
            .get_long_about()
            .expect("the struct doc is rendered as long_about")
            .to_string();
        assert!(
            long_about.to_lowercase().contains("session"),
            "C-043: the long help must say the opt-out also suppresses the session PATH:\n{long_about}"
        );

        let flag_help = command
            .get_arguments()
            .find(|argument| argument.get_long() == Some("no-modify-path"))
            .and_then(|argument| argument.get_help().map(ToString::to_string))
            .expect("the flag carries help text");
        assert!(
            flag_help.to_lowercase().contains("session"),
            "C-043: the flag's own help must say the same:\n{flag_help}"
        );
    }

    /// C-037 / C-036: the rendered exit-code table gains **one** row — an
    /// unencodable `$OCX_HOME` is 78 — and must **not** gain a row for a
    /// session-PATH write failure, because C-036 makes that exit 0.
    ///
    /// A wrong row here is the same Block-tier defect as wrong help text: it is
    /// the surface a script author reads to decide what to branch on.
    #[test]
    fn the_exit_code_table_carries_the_encoding_refusal_and_not_the_write_failure() {
        let long_about = <SelfSetup as clap::CommandFactory>::command()
            .get_long_about()
            .expect("the struct doc is rendered as long_about")
            .to_string();

        // clap re-wraps the doc comment, so the markdown table arrives as one
        // line: split it back into rows on the `| |` cell boundary rather than
        // on newlines, which are gone by the time the help is rendered.
        let table = long_about
            .split("# Exit codes")
            .nth(1)
            .expect("the exit-code table is part of the long help");
        let path_rows: Vec<&str> = table
            .split("| |")
            .filter(|row| row.to_lowercase().contains("session"))
            .collect();
        assert_eq!(
            path_rows.len(),
            1,
            "exactly one session-PATH row belongs in the table, the encoding refusal: {path_rows:?}"
        );
        assert!(
            path_rows[0].contains("78"),
            "an unencodable `$OCX_HOME` is a configuration fault, exit 78: {}",
            path_rows[0]
        );
    }

    /// C-036: `exit_code_for` must **not** learn about session-PATH outcomes.
    ///
    /// A `Failed` is warned about and exits 0; a `SkippedUnsupported` is
    /// ordinary. The only session-PATH condition that changes the exit code is
    /// C-037's refusal, and that travels the `Err` arm of `setup::run` through
    /// `ClassifyExitCode`, never this function.
    ///
    /// A tripwire for the likely accident — the surrounding phases all feed
    /// this function, so adding one more looks like the local convention.
    #[test]
    fn the_exit_code_does_not_depend_on_a_session_path_outcome() {
        let source = include_str!("setup.rs");
        let body = source
            .split("fn exit_code_for(")
            .nth(1)
            .expect("exit_code_for exists")
            .split("\n}")
            .next()
            .expect("its body terminates");

        assert!(
            !body.contains("session_path"),
            "C-036: a session-PATH outcome never changes the exit code:\n{body}"
        );
    }
}
