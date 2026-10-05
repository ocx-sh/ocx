// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use ocx_config::ConfigTier;
use ocx_config::env;
use ocx_config::shell::ShellConfig;
use ocx_exit::ExitCode as OcxExitCode;
use ocx_project::activate::ActivateMode;
use ocx_setup as setup;
use ocx_setup::shell_config::{self, ShellKey, ShellValue};
use ocx_setup::{ExtraCaCertsOutcome, SessionPathOutcome, SetupOptions, SetupOutcome, VersionSpec};
use ocx_util::boolean_string::BooleanString;

use crate::api::data::self_setup::SelfSetupData;
use crate::command::config_setup::resolve_managed_config_arg;
use crate::options::{ModifyPath, Profiles};

/// Arguments of `ocx self setup`.
///
/// An edited managed profile fence (`# >>> ocx v1 <hash> >>>`) is reported dirty
/// and left alone (exit 82) unless `--force`. A `[shell]` toggle writes one key and
/// keeps the rest of `config.toml`, comments included; it is never reported dirty,
/// and a tier above `$OCX_HOME` still wins. A Windows `Restricted` execution policy
/// makes the profile block inert; setup says how to relax it, never changes it.
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
// Rustdoc only: clap renders `ocx self setup --help` from the `SelfGroup::Setup` variant.
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
    #[arg(long_help = "\
        Record how a toolchain reaches PATH: writes `activate` to `$OCX_HOME/ocx.toml`.\n\n\
        `env` composes the toolchain environment on every prompt. `bin` puts the toolchain's `bin` \
        directory on PATH and composes nothing else, so each binary is resolved by its launcher \
        when it runs. `none` composes nothing and adds nothing.\n\n\
        For the global toolchain this flag writes, `bin` and `none` leave the same PATH: \
        `$OCX_HOME/toolchain/active/bin` is a session directory `ocx self setup` registers once and \
        no prompt withdraws, so the global binaries stay reachable through their trampolines under \
        either. The two values part company only for a project's own toolchain.\n\n\
        Omit to leave `ocx.toml` untouched; the file is created carrying only this key if it does \
        not exist yet. Writes the global toolchain's setting - a project's own `ocx.toml` still \
        decides for that project, and `OCX_TOOLCHAIN_ACTIVATE` is the weakest tier of all, \
        consulted only when no file sets the key.")]
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
    #[arg(value_name = "VERSION", value_parser = |s: &str| VersionSpec::from_str(s).map_err(|e| e.to_string()))]
    version: Option<VersionSpec>,

    // Read only through `resolve_modify_path`, or the env and config rungs are skipped.
    #[clap(flatten)]
    modify_path: ModifyPath,

    // Read only through `Profiles::explicit`, above `[shell] profiles` and auto-detect.
    #[clap(flatten)]
    profiles: Profiles,

    /// Report the intended actions without writing anything.
    #[arg(long)]
    dry_run: bool,

    /// Overwrite a managed block that carries user edits (the dirty state).
    #[arg(long)]
    force: bool,

    /// Adopt (or clear) the corporate managed-config tier.
    #[arg(long_help = "\
        Adopt (or clear) the corporate managed-config tier.\n\n\
        Resolves an OCI reference to a managed-config artifact, synchronously fetches and persists \
        a snapshot, and only then writes the `[managed]` seed fence in `$OCX_HOME/config.toml` - a \
        fetch failure leaves no partial state. Pass an empty string (`--managed-config \"\"`) to \
        clear an existing seed and delete the snapshot.\n\n\
        Precedence when omitted: `OCX_MANAGED_CONFIG` env var, then the existing seed. Omit \
        entirely to leave the managed-config tier untouched.\n\n\
        Every run reconciles an already-adopted seed against the registry, so a newer published \
        config is picked up here too. If that refresh cannot reach the registry, the existing \
        snapshot is kept and setup still succeeds.")]
    #[arg(long, value_name = "REF")]
    managed_config: Option<String>,

    /// Spawned by `ocx self update`'s post-swap refresh: heal profiles,
    /// introduce none, and write no `config.toml` key.
    ///
    /// Hidden: it is machine surface, not something to type.
    #[clap(long = "handoff", hide = true)]
    handoff: bool,
}

impl SelfSetup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Before the bootstrap, so a registry failure cannot drop the requested toggle.
        self.apply_shell_flags(&context).await?;
        self.apply_toolchain_activate(&context).await?;

        let managed_config = resolve_managed_config_arg(
            self.managed_config.as_deref(),
            context.config(),
            context.managed_config_env_override(),
        )?;
        let shell = context.config().shell.as_ref();
        let options = SetupOptions {
            no_modify_path: resolve_modify_path(self.modify_path.explicit(), shell.and_then(|shell| shell.modify_path)),
            profiles: self
                .profiles
                .explicit()
                .or_else(|| shell.and_then(|shell| shell.profiles.clone())),
            dry_run: self.dry_run,
            force: self.force,
            handoff: self.handoff,
            version: self.version.clone(),
            managed_config,
        };

        let outcome = setup::run(&options, context.config(), context.manager(), context.file_structure()).await?;

        emit_advisories(&context, &outcome, self.dry_run);

        let exit = exit_code_for(&outcome, self.force, self.dry_run);

        context.api().report(&SelfSetupData::from_outcome(&outcome))?;
        Ok(exit)
    }

    /// Write the requested `[shell]` toggles into `$OCX_HOME/config.toml`, warning when a higher tier still decides.
    ///
    /// `--config` and `OCX_CONFIG` never redirect this write; a failure is 74, never 82.
    async fn apply_shell_flags(&self, context: &crate::app::Context) -> anyhow::Result<()> {
        let writes = shell_writes(self);
        if writes.is_empty() {
            return Ok(());
        }
        let config_path = context.file_structure().root().join("config.toml");

        for (key, value) in writes {
            if self.dry_run {
                context.ui().status(
                    "Setup",
                    format!(
                        "would set [shell] {key} = {value} in {path}",
                        key = key.key(),
                        value = describe_shell_write_value(&value),
                        path = config_path.display()
                    ),
                );
            } else {
                shell_config::set(
                    &context.file_structure().locks,
                    &config_path,
                    key,
                    value.as_shell_value(),
                )
                .await?;
            }

            // Outside the dry-run guard: which tier decides is exactly what `--dry-run` answers.
            if let Some(tier) = overriding_tier(context.config().shell.as_ref(), key) {
                context.ui().warn(format!(
                    "[shell] {key} is also set by {tier}, which wins over {path} - the value {written} will not take effect",
                    key = key.key(),
                    path = config_path.display(),
                    written = if self.dry_run { "this would write" } else { "just written" },
                ));
            }
        }
        Ok(())
    }

    /// Write the requested `activate` mode into the global `$OCX_HOME/ocx.toml`, never the project in effect.
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
        ocx_project::set_activate(&config_path, &context.file_structure().locks, mode).await?;
        Ok(())
    }
}

/// The value one [`ShellKey`] write carries, owned because [`ShellValue::Paths`] only borrows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellWriteValue {
    Bool(bool),
    Paths(Vec<PathBuf>),
}

impl ShellWriteValue {
    /// Borrow this value as the [`ShellValue`] [`shell_config::set`] takes.
    fn as_shell_value(&self) -> ShellValue<'_> {
        match self {
            Self::Bool(value) => ShellValue::Bool(*value),
            Self::Paths(paths) => ShellValue::Paths(paths),
        }
    }
}

/// Render a [`ShellWriteValue`] for the `--dry-run` status line.
fn describe_shell_write_value(value: &ShellWriteValue) -> String {
    match value {
        ShellWriteValue::Bool(value) => value.to_string(),
        ShellWriteValue::Paths(paths) => format!("{paths:?}"),
    }
}

/// The `[shell]` writes this invocation asked for; only a typed key writes, so a bare run changes nothing.
///
/// The `--handoff` guard lives here, not in `execute`, so no future caller persists by skipping it.
fn shell_writes(setup: &SelfSetup) -> Vec<(ShellKey, ShellWriteValue)> {
    if setup.handoff {
        return Vec::new();
    }
    [
        requested(setup.hook, setup.no_hook).map(|value| (ShellKey::Hook, ShellWriteValue::Bool(value))),
        requested(setup.completion, setup.no_completion)
            .map(|value| (ShellKey::Completions, ShellWriteValue::Bool(value))),
        setup
            .modify_path
            .explicit()
            .map(|value| (ShellKey::ModifyPath, ShellWriteValue::Bool(value))),
        setup
            .profiles
            .explicit()
            .map(|value| (ShellKey::Profiles, ShellWriteValue::Paths(value))),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Collapse one `--X` / `--no-X` pair into its requested value; off wins a tie, as in `options::Hook`.
fn requested(on: bool, off: bool) -> Option<bool> {
    match (on, off) {
        (_, true) => Some(false),
        (true, false) => Some(true),
        (false, false) => None,
    }
}

/// Read `OCX_NO_MODIFY_PATH` as a tri-state in `modify_path`'s positive sense; `None` when absent or unrecognised.
///
/// Not [`ocx_util::env::flag`]: it folds both into a default, so a lower rung could never answer.
fn env_modify_path() -> Option<bool> {
    let raw = ocx_util::env::var(env::keys::OCX_NO_MODIFY_PATH)?;
    match BooleanString::try_from(raw.as_str()) {
        Ok(boolean) => Some(!bool::from(boolean)),
        Err(error) => {
            log::warn!(
                "environment variable '{}' has invalid boolean value: {error}",
                env::keys::OCX_NO_MODIFY_PATH
            );
            None
        }
    }
}

/// Resolve the `modify_path` ladder (flag, env, `[shell] modify_path`, default) and return its negation.
///
/// The env is read here, not in [`ModifyPath`], where it would outrank the config rung.
fn resolve_modify_path(explicit: Option<bool>, configured: Option<bool>) -> bool {
    !explicit.or_else(env_modify_path).or(configured).unwrap_or(true)
}

/// The tier that will still decide `key` after the home-tier write lands, or
/// `None` when the write itself decides.
fn overriding_tier(shell: Option<&ShellConfig>, key: ShellKey) -> Option<ConfigTier> {
    let shell = shell?;
    let tier = match key {
        ShellKey::Hook => shell.hook_tier,
        ShellKey::Completions => shell.completions_tier,
        ShellKey::ModifyPath => shell.modify_path_tier,
        ShellKey::Profiles => shell.profiles_tier,
    }?;
    // `ConfigTier`'s order is the fold order, so ranking above Home means still deciding.
    (tier > ConfigTier::Home).then_some(tier)
}

/// Print the non-fatal advisories to stderr.
///
/// Under `dry_run` the session-PATH writes are previewed, or a `written` row reads as a completed write.
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
                // `{:?}`, not `Display`: a raw newline in an unvalidated path would forge an advisory line (CWE-117).
                context
                    .ui()
                    .status("Setup", format!("would register {directory:?} in {location:?}"));
            }
        }
    }
    // A session-PATH write failure exits 0, so this warning is its only trace.
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
    if matches!(outcome.extra_ca_certs, ExtraCaCertsOutcome::SystemLocked) {
        context.ui().warn(format!(
            "OCX_EXTRA_CA_CERTS was not persisted: extra_ca_certs / extra_ca_certs_pem are locked by {}; edit the \
             system tier or ask its owner",
            ocx_config::loader::ConfigLoader::system_path().display(),
        ));
    }
    // One line per changed surface: a profile is re-sourced, a session PATH needs a new login.
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
    // macOS names an unbundled login item after its executable, so the agent shows up as `sh`.
    if !dry_run && let Some(plist) = setup::launch_agent_written(&outcome.session_path) {
        context.ui().status(
            "Setup",
            format!("macOS lists the session-PATH agent as \"sh\" under Login Items; that entry is ocx's LaunchAgent {plist:?}"),
        );
    }
}

/// Decide the exit code: [`OcxExitCode::DirtyRcBlock`] (82) for a dirty profile or `[managed]` fence left
/// untouched, never under `--force` or `--dry-run`.
fn exit_code_for(outcome: &SetupOutcome, force: bool, dry_run: bool) -> ExitCode {
    let profile_dirty = setup::profiles_dirty(&outcome.profiles);
    let managed_config_dirty = matches!(outcome.managed_config, ocx_setup::ManagedConfigSetupOutcome::Dirty);
    if (profile_dirty || managed_config_dirty) && !force && !dry_run {
        return OcxExitCode::DirtyRcBlock.into();
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser as _;
    use ocx_setup::{
        BootstrapOutcome, BootstrapStatus, ExtraCaCertsOutcome, ManagedConfigSetupOutcome, ProfileOutcome, SetupOutcome,
    };

    use super::*;

    fn parse(args: &[&str]) -> SelfSetup {
        SelfSetup::try_parse_from(std::iter::once("setup").chain(args.iter().copied())).expect("valid grammar")
    }

    fn stamped(key: ShellKey, tier: ConfigTier) -> ShellConfig {
        let mut shell = ShellConfig::default();
        match key {
            ShellKey::Hook => {
                shell.hook = Some(true);
                shell.hook_tier = Some(tier);
            }
            ShellKey::Completions => {
                shell.completions = Some(true);
                shell.completions_tier = Some(tier);
            }
            ShellKey::ModifyPath => {
                shell.modify_path = Some(true);
                shell.modify_path_tier = Some(tier);
            }
            ShellKey::Profiles => {
                shell.profiles = Some(Vec::new());
                shell.profiles_tier = Some(tier);
            }
        }
        shell
    }

    /// Extract the bool out of a [`ShellWriteValue`] that is known to be one —
    /// every write `shell_writes` produces from a `--hook`/`--completion`
    /// flag is a `Bool`, never a `Paths`.
    fn as_bool(value: &ShellWriteValue) -> bool {
        match value {
            ShellWriteValue::Bool(value) => *value,
            ShellWriteValue::Paths(paths) => panic!("expected a bool write, got Paths({paths:?})"),
        }
    }

    /// The load-bearing negative: `ocx self setup` with neither flag of a
    /// pair requests no write at all, so `config.toml` is left
    /// byte-identical.
    #[test]
    fn neither_flag_requests_no_write() {
        assert!(shell_writes(&parse(&[])).is_empty());
        assert!(
            shell_writes(&parse(&["1.2.3", "--dry-run"])).is_empty(),
            "an unrelated flag or the positional must not conjure a [shell] write"
        );
    }

    /// Each pair writes its own key, in both directions.
    #[test]
    fn each_pair_writes_its_own_key_in_both_directions() {
        for (args, expected) in [
            (vec!["--hook"], vec![(ShellKey::Hook, ShellWriteValue::Bool(true))]),
            (vec!["--no-hook"], vec![(ShellKey::Hook, ShellWriteValue::Bool(false))]),
            (
                vec!["--completion"],
                vec![(ShellKey::Completions, ShellWriteValue::Bool(true))],
            ),
            (
                vec!["--no-completion"],
                vec![(ShellKey::Completions, ShellWriteValue::Bool(false))],
            ),
            (
                vec!["--no-hook", "--completion"],
                vec![
                    (ShellKey::Hook, ShellWriteValue::Bool(false)),
                    (ShellKey::Completions, ShellWriteValue::Bool(true)),
                ],
            ),
        ] {
            assert_eq!(shell_writes(&parse(&args)), expected, "for {args:?}");
        }
    }

    /// The pairs are POSIX last-wins, so passing both is not an error —
    /// `overrides_with` clears the loser, and the survivor decides.
    #[test]
    fn a_repeated_pair_is_last_wins_not_an_error() {
        assert_eq!(
            shell_writes(&parse(&["--hook", "--no-hook"])),
            vec![(ShellKey::Hook, ShellWriteValue::Bool(false))]
        );
        assert_eq!(
            shell_writes(&parse(&["--no-hook", "--hook"])),
            vec![(ShellKey::Hook, ShellWriteValue::Bool(true))]
        );
        assert_eq!(
            shell_writes(&parse(&["--completion", "--no-completion"])),
            vec![(ShellKey::Completions, ShellWriteValue::Bool(false))]
        );
        assert_eq!(
            shell_writes(&parse(&["--no-completion", "--completion"])),
            vec![(ShellKey::Completions, ShellWriteValue::Bool(true))]
        );
    }

    /// The flags sit before the positional and are booleans, so VERSION
    /// is never swallowed by one of them.
    #[test]
    fn the_positional_survives_a_preceding_flag() {
        let parsed = parse(&["--hook", "1.2.3"]);
        assert_eq!(
            parsed.version.as_ref().map(ToString::to_string),
            Some("1.2.3".to_owned())
        );
        assert_eq!(
            shell_writes(&parsed),
            vec![(ShellKey::Hook, ShellWriteValue::Bool(true))]
        );
    }

    /// A tier above home still decides after the write, and
    /// it is named by the tier that actually set the key — never a hard-coded
    /// "managed".
    #[test]
    fn a_higher_tier_is_reported_by_name() {
        for tier in [ConfigTier::Managed, ConfigTier::Explicit] {
            for key in [
                ShellKey::Hook,
                ShellKey::Completions,
                ShellKey::ModifyPath,
                ShellKey::Profiles,
            ] {
                assert_eq!(
                    overriding_tier(Some(&stamped(key, tier)), key),
                    Some(tier),
                    "for {key:?}"
                );
            }
        }
    }

    /// A tier at or below home loses to the home-tier write, so there is
    /// nothing to report — and neither does a key no tier set.
    #[test]
    fn home_and_below_are_not_reported() {
        for tier in [ConfigTier::System, ConfigTier::User, ConfigTier::Home] {
            assert_eq!(
                overriding_tier(Some(&stamped(ShellKey::Hook, tier)), ShellKey::Hook),
                None
            );
        }
        assert_eq!(overriding_tier(None, ShellKey::Hook), None);
        assert_eq!(
            overriding_tier(Some(&ShellConfig::default()), ShellKey::Hook),
            None,
            "an unset key has no deciding tier"
        );
    }

    /// Drift guard: `overriding_tier` answers **per key**, not
    /// per shared `_tier` field. Four differently-set tiers on one
    /// `ShellConfig`, each reported only for its own key.
    ///
    /// Red-state: point `ShellKey::ModifyPath` at `shell.profiles_tier` (or
    /// any other cross-wiring of two `_tier` fields) and the `ModifyPath` /
    /// `Profiles` assertions swap answers.
    #[test]
    fn overriding_tier_answers_per_key() {
        let shell = ShellConfig {
            hook: Some(true),
            hook_tier: Some(ConfigTier::Home),
            completions: Some(true),
            completions_tier: None,
            modify_path: Some(true),
            modify_path_tier: Some(ConfigTier::Managed),
            profiles: Some(Vec::new()),
            profiles_tier: Some(ConfigTier::Explicit),
            ..ShellConfig::default()
        };

        assert_eq!(
            overriding_tier(Some(&shell), ShellKey::Hook),
            None,
            "Home is not above Home"
        );
        assert_eq!(
            overriding_tier(Some(&shell), ShellKey::Completions),
            None,
            "no tier set this key at all"
        );
        assert_eq!(
            overriding_tier(Some(&shell), ShellKey::ModifyPath),
            Some(ConfigTier::Managed)
        );
        assert_eq!(
            overriding_tier(Some(&shell), ShellKey::Profiles),
            Some(ConfigTier::Explicit)
        );
    }

    /// Drift guard: the four long flags `self setup` declares are the
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

    /// Drift guard, second half: `requested`'s tie-break is a hand copy
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
                shell_writes(&parse(&args)).first().map(|(_, value)| as_bool(value)),
                shared_flag,
                "for {args:?}"
            );
        }
    }

    /// The provenance is per key — a managed `hook` says nothing about
    /// who decides `completions`.
    #[test]
    fn the_report_is_per_key() {
        let shell = stamped(ShellKey::Hook, ConfigTier::Managed);
        assert_eq!(overriding_tier(Some(&shell), ShellKey::Hook), Some(ConfigTier::Managed));
        assert_eq!(overriding_tier(Some(&shell), ShellKey::Completions), None);
    }

    // ── Task 3: persisting `modify_path` / `profiles`, and the hand-off guard ──

    /// Task 3: an explicit `--no-modify-path` persists the key; a bare run
    /// does not create or touch the file at all — same load-bearing shape as
    /// `neither_flag_requests_no_write`, pinned separately for the two new
    /// keys.
    ///
    /// Red-state: make the `modify_path` write unconditional (drop the
    /// `explicit()` check in `shell_writes`) and the second assertion fails —
    /// a bare run would then write `modify_path = true`.
    #[test]
    fn explicit_flag_persists_but_silence_does_not() {
        assert_eq!(
            shell_writes(&parse(&["--no-modify-path"])),
            vec![(ShellKey::ModifyPath, ShellWriteValue::Bool(false))]
        );
        assert!(
            shell_writes(&parse(&[])).is_empty(),
            "a bare run must not conjure a modify_path or profiles write"
        );
    }

    /// Task 3: `--handoff` persists nothing, however many opt-outs the same
    /// invocation carries.
    ///
    /// Red-state: remove the `if setup.handoff { return Vec::new(); }` guard
    /// from `shell_writes` and this test fails — `modify_path` and `profiles`
    /// both appear in the write set.
    #[test]
    fn handoff_writes_no_config() {
        assert!(
            shell_writes(&parse(&["--handoff", "--no-modify-path", "--profile", "x", "--hook"])).is_empty(),
            "a --handoff run must persist nothing, however many opt-outs or toggles it carries"
        );
    }

    /// Task 1 ladder-order coverage: flag beats env, env beats config, config
    /// beats default, most specific first — the same shape
    /// `options::hook::resolve_ladder`'s rung tests pin for `[shell] hook` /
    /// `[shell] completions`.
    ///
    /// The only test in this file touching `OCX_NO_MODIFY_PATH`'s real
    /// process environment; same single-`#[test]` precedent as
    /// `options::hook::tests::each_ladder_reads_its_own_environment_key`.
    ///
    /// Red-state: swap two arms of the `.or_else(...).or(...)` chain in
    /// `resolve_modify_path` and the assertion for the swapped pair
    /// disagrees.
    #[test]
    fn modify_path_ladder_order() {
        // SAFETY: see the doc comment above — this is the one test that
        // touches this key.
        unsafe { std::env::remove_var(env::keys::OCX_NO_MODIFY_PATH) };

        // Rung 4 (the floor): nothing set anywhere -> modify allowed.
        assert!(!resolve_modify_path(None, None), "the default is to modify the PATH");

        // Rung 3: `[shell] modify_path` decides once the flag and env are
        // silent, in both directions.
        assert!(
            resolve_modify_path(None, Some(false)),
            "`[shell] modify_path = false` must turn on no_modify_path over the default"
        );
        assert!(
            !resolve_modify_path(None, Some(true)),
            "`[shell] modify_path = true` must keep no_modify_path off"
        );

        // Rung 2: the environment outranks the config file, in both
        // directions.
        // SAFETY: see above.
        unsafe { std::env::set_var(env::keys::OCX_NO_MODIFY_PATH, "1") };
        assert!(
            resolve_modify_path(None, Some(true)),
            "OCX_NO_MODIFY_PATH=1 must outrank `[shell] modify_path = true`"
        );
        // SAFETY: see above.
        unsafe { std::env::set_var(env::keys::OCX_NO_MODIFY_PATH, "0") };
        assert!(
            !resolve_modify_path(None, Some(false)),
            "OCX_NO_MODIFY_PATH=0 must outrank `[shell] modify_path = false`"
        );

        // Rung 1: the CLI flag outranks the environment.
        assert!(
            resolve_modify_path(Some(false), Some(true)),
            "the explicit flag must outrank both an env override and the config"
        );

        // SAFETY: see above.
        unsafe { std::env::remove_var(env::keys::OCX_NO_MODIFY_PATH) };
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
            extra_ca_certs: ExtraCaCertsOutcome::NotConfigured,
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

    // ── `--toolchain-activate` and the PATH opt-out ─────────────────────

    /// The load-bearing negative, in the same shape as
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

    /// The flag accepts exactly the three wire spellings
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

    /// An unknown mode is a clap usage error — exit 64 — rather
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

    /// The flag takes a value, so the VERSION positional is never
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

    /// The flag's help states a **promise about behaviour**, not just about
    /// a file write, and the promise is now kept on both tiers — so the text is
    /// pinned and the next person to change the behaviour has to change the
    /// promise too.
    ///
    /// Every clause is load-bearing: the global toolchain reads its own
    /// `activate` (`ocx_cli::command::self_group::activate`'s
    /// `global_prompt_entries`), a project's own `ocx.toml` still decides for
    /// that project (`ocx_package_manager::activation::project_contribution`), and
    /// `OCX_TOOLCHAIN_ACTIVATE` is the weakest tier on both
    /// (`ocx_package_manager::activation::activate_mode`). `quality-cli-help.md` rates an
    /// incorrect statement of behaviour in clap-rendered text Block-tier.
    ///
    /// The second literal is the bin/none PATH equivalence's consequence, pinned close to
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
                 registers once and no prompt withdraws, so the global binaries stay reachable \
                 through their \
                 trampolines under either. The two values part company only for a project's own \
                 toolchain."
            ),
            "the bin/none PATH equivalence must be stated in the help closest to the flag, not only in the \
             website reference; `none` that claimed to compose nothing AND add nothing was false \
             for the one tier this flag writes:\n{rendered}"
        );
    }

    /// The opt-out is **asymmetric by construction**, and this
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
            "adding `--modify-path` would change the precedence answer: {longs:?}"
        );
    }

    /// `--no-modify-path` now suppresses the **whole
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
            "the long help must say the opt-out also suppresses the session PATH:\n{long_about}"
        );

        let flag_help = command
            .get_arguments()
            .find(|argument| argument.get_long() == Some("no-modify-path"))
            .and_then(|argument| argument.get_help().map(ToString::to_string))
            .expect("the flag carries help text");
        assert!(
            flag_help.to_lowercase().contains("session"),
            "the flag's own help must say the same:\n{flag_help}"
        );
    }

    /// The rendered exit-code table gains **one** row — an
    /// unencodable `$OCX_HOME` is 78 — and must **not** gain a row for a
    /// session-PATH write failure, because that outcome is already exit 0.
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

    /// `exit_code_for` must **not** learn about session-PATH outcomes.
    ///
    /// A `Failed` is warned about and exits 0; a `SkippedUnsupported` is
    /// ordinary. The only session-PATH condition that changes the exit code is
    /// the unencodable-`$OCX_HOME` refusal, which travels the `Err` arm of `setup::run` through
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
            "a session-PATH outcome never changes the exit code:\n{body}"
        );
    }
}
