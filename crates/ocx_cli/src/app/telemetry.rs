// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Which commands may export usage records, and the slot an exporting run carries.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ocx_config::telemetry::TelemetryConfig;
use ocx_console::UserInterface;
use ocx_package_manager::activity::{Action, ActivityLog, PackageActivity, Role};
use ocx_package_manager::record::purl::package_url;
use ocx_telemetry::{
    CommandOutcome, CommandResult, Decision, PackageAction, PackageRecordInput, PackageRole, ResourceInputs, Slot,
};
use ocx_util::tls::ExtraRoots;

use crate::command::Command;
use crate::command::leaf::Leaf;
use crate::command::package::Package;

/// Whether a command may export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The arming gate decides.
    Send,
    /// The gate is never consulted and nothing is read, written or opened.
    Never,
}

/// The class of a command: `leaf` is `None` for an external plugin, and `shell_flag` is whether
/// `env` or `package env` runs with `--shell`, whose output is evaluated by the user's shell.
///
/// An exhaustive match over [`Leaf`], so a new leaf is a compile error until its class is decided.
pub fn class(leaf: Option<Leaf>, shell_flag: bool) -> Class {
    let Some(leaf) = leaf else {
        return Class::Never;
    };
    match leaf {
        Leaf::Exec
        | Leaf::Run
        | Leaf::PackageExec
        | Leaf::LauncherExec
        | Leaf::LauncherShim
        | Leaf::Shell
        | Leaf::ShellState
        | Leaf::DirenvExport
        | Leaf::SelfActivate
        | Leaf::ShellCompletion
        | Leaf::Version
        | Leaf::Status
        | Leaf::ConfigTest => Class::Never,
        Leaf::Env | Leaf::PackageEnv if shell_flag => Class::Never,
        Leaf::Env
        | Leaf::PackageEnv
        | Leaf::About
        | Leaf::Add
        | Leaf::Clean
        | Leaf::ConfigPush
        | Leaf::ConfigSetup
        | Leaf::ConfigUpdate
        | Leaf::DirenvInit
        | Leaf::IndexCatalog
        | Leaf::IndexList
        | Leaf::IndexRegenerate
        | Leaf::IndexSync
        | Leaf::IndexUpdate
        | Leaf::Init
        | Leaf::Inspect
        | Leaf::Lock
        | Leaf::Login
        | Leaf::Logout
        | Leaf::PackageAnnounce
        | Leaf::PackageAttest
        | Leaf::PackageCascadeCheck
        | Leaf::PackageCascadeRepair
        | Leaf::PackageClaim
        | Leaf::PackageCopy
        | Leaf::PackageCreate
        | Leaf::PackageDeps
        | Leaf::PackageDescribe
        | Leaf::PackageDescriptionPull
        | Leaf::PackageDescriptionPush
        | Leaf::PackageDeselect
        | Leaf::PackageInfo
        | Leaf::PackageInspect
        | Leaf::PackageInstall
        | Leaf::PackagePrune
        | Leaf::PackagePull
        | Leaf::PackagePush
        | Leaf::PackageReceipt
        | Leaf::PackageSbom
        | Leaf::PackageSelect
        | Leaf::PackageSign
        | Leaf::PackageTest
        | Leaf::PackageUninstall
        | Leaf::PackageVerify
        | Leaf::PackageWhich
        | Leaf::PatchFreeze
        | Leaf::PatchPublish
        | Leaf::PatchSync
        | Leaf::PatchTest
        | Leaf::PatchWhy
        | Leaf::Pull
        | Leaf::Remove
        | Leaf::SelfSetup
        | Leaf::SelfUpdate
        | Leaf::ShellAllow
        | Leaf::ShellRevoke
        | Leaf::Update
        | Leaf::Upgrade => Class::Send,
    }
}

/// The class of the command line `ocx` parsed; `None` (no subcommand) never exports.
pub fn command_class(command: Option<&Command>) -> Class {
    let shell_flag = match command {
        Some(Command::Env(env)) => env.shell_requested(),
        Some(Command::Package(Package::Env(env))) => env.shell_requested(),
        _ => false,
    };
    match command {
        Some(command) => class(command.leaf(), shell_flag),
        None => Class::Never,
    }
}

/// The command's name in the record: the words after `ocx` that select its leaf, so `direnv init`
/// stays distinct from `direnv export`. Empty for an external plugin, which never sends.
pub fn command_name(command: &Command) -> String {
    command.leaf().map(|leaf| leaf.path().join(" ")).unwrap_or_default()
}

/// The process exit code as an integer. `std::process::ExitCode` exposes none, but it is `PartialEq`,
/// so the value is the one number whose `ExitCode` equals it; every `u8` is covered.
pub fn exit_code_value(code: ExitCode) -> u8 {
    (0..=u8::MAX)
        .find(|value| ExitCode::from(*value) == code)
        .unwrap_or(ocx_exit::ExitCode::Failure as u8)
}

/// What the record says about the run: the exit code and slug of an error come from the same
/// classification the process boundary uses, and an `Ok` is a success whatever integer it carries,
/// since a child's own exit code is not an ocx failure.
pub fn command_outcome(name: String, duration: Duration, result: &anyhow::Result<ExitCode>) -> CommandOutcome {
    match result {
        Ok(code) => CommandOutcome {
            name,
            duration,
            exit_code: exit_code_value(*code),
            result: CommandResult::Succeeded,
        },
        Err(error) => {
            let (code, detail) = crate::exit::classify_decision(error.as_ref());
            CommandOutcome {
                name,
                duration,
                exit_code: code as u8,
                result: CommandResult::Failed {
                    detail: detail.map(str::to_owned),
                },
            }
        }
    }
}

/// Drains the command's package activity into records, once the command and any update apply are done.
///
/// An entry with no logical identity (no registry or repository to name) has no purl and no record.
pub fn package_records(log: &ActivityLog) -> Vec<PackageRecordInput> {
    log.drain().into_iter().filter_map(package_record).collect()
}

fn package_record(activity: PackageActivity) -> Option<PackageRecordInput> {
    let purl = package_url(&activity.pinned, activity.platform.as_ref())?;
    let action = match activity.action {
        Action::Installed => PackageAction::Installed,
        Action::Pulled => PackageAction::Pulled,
        Action::Removed => PackageAction::Removed,
    };
    let role = match activity.role {
        Role::Root => PackageRole::Root,
        Role::Dependency => PackageRole::Dependency,
    };
    // A store hit and a removal fetched nothing, so they carry no size rather than a zero.
    let fetched = !activity.store_hit && action != PackageAction::Removed;
    Some(PackageRecordInput {
        purl,
        action,
        role,
        store_hit: activity.store_hit,
        download_size: fetched.then_some(activity.download_size),
        resolve: activity.resolve,
        download: activity.download,
        extract: activity.extract,
    })
}

/// An armed run's export slot, and whether its package manager must log package activity.
pub struct Arming {
    /// Holds the decision until `run_from` hands it the outcome.
    pub slot: Slot,
    /// `true` only when the run may send: an unarmed run, or one without a destination, records nothing.
    pub records_packages: bool,
}

/// Decides this run's export and builds its slot, printing the gate's warnings once to stderr.
///
/// Called once the merged config and the extra CA roots are known. Nothing is probed here: the
/// resource producer runs inside [`Slot::finish`], and only for a run with a destination.
pub fn arm(
    section: Option<&TelemetryConfig>,
    offline: bool,
    installation_id_file: PathBuf,
    extra_roots: ExtraRoots,
    ui: &UserInterface,
) -> Arming {
    let decision = ocx_telemetry::decide(section, offline);
    let (warnings, records_packages) = match &decision {
        Decision::Off { warnings, .. } => (warnings, false),
        Decision::Armed {
            warnings, destination, ..
        } => (warnings, destination.is_some()),
    };
    for warning in warnings {
        ui.warn(warning);
    }
    // Only a run that can send ever calls the producer, so only it pays for the clones.
    let slot = if records_packages {
        let service_name = section.and_then(|section| section.service_name.clone());
        let attributes = section
            .and_then(|section| section.resource_attributes.clone())
            .unwrap_or_default();
        Slot::new(
            decision,
            move || resource_inputs(service_name, attributes),
            installation_id_file,
            extra_roots,
        )
    } else {
        Slot::new(
            decision,
            || resource_inputs(None, Default::default()),
            installation_id_file,
            extra_roots,
        )
    };
    Arming { slot, records_packages }
}

/// Probes the process for the resource attributes; every lookup is best-effort and drops its own key.
fn resource_inputs(
    config_service_name: Option<String>,
    config_attributes: std::collections::BTreeMap<String, String>,
) -> ResourceInputs {
    let mut inputs = ResourceInputs {
        version: super::version().to_owned(),
        pid: std::process::id(),
        parent_pid: ocx_package_manager::record::environment::parent_pid(),
        host_name: ocx_package_manager::record::environment::host().name,
        os_type: ocx_oci::OperatingSystem::current().map(|os| os.to_string()),
        arch: ocx_oci::Architecture::current().map(|arch| arch.to_string()),
        interactive: console::Term::stderr().is_term(),
        config_service_name,
        config_attributes,
        ..ResourceInputs::default()
    };
    inputs.read_environment();
    inputs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Commands whose output is read by a shell, a launcher or a script, or that report on
    /// ocx itself, so they never export.
    const NEVER: &[Leaf] = &[
        Leaf::Exec,
        Leaf::Run,
        Leaf::PackageExec,
        Leaf::LauncherExec,
        Leaf::LauncherShim,
        Leaf::Shell,
        Leaf::ShellState,
        Leaf::DirenvExport,
        Leaf::SelfActivate,
        Leaf::ShellCompletion,
        Leaf::Version,
        Leaf::Status,
        Leaf::ConfigTest,
    ];

    #[test]
    fn every_leaf_has_a_telemetry_class() {
        for &leaf in Leaf::ALL {
            let expected = if NEVER.contains(&leaf) {
                Class::Never
            } else {
                Class::Send
            };
            assert_eq!(class(Some(leaf), false), expected, "{leaf:?}");
        }
        for leaf in [Leaf::Env, Leaf::PackageEnv] {
            assert_eq!(class(Some(leaf), true), Class::Never, "{leaf:?} with --shell");
            assert_eq!(class(Some(leaf), false), Class::Send, "{leaf:?} without --shell");
        }
        assert_eq!(class(None, false), Class::Never, "an external plugin");
    }

    fn class_of(words: &[&str]) -> Class {
        use clap::Parser as _;
        let cli = crate::app::Cli::try_parse_from(words).expect("the fixture is a valid invocation");
        command_class(cli.command.as_ref())
    }

    #[test]
    fn the_shell_flag_of_both_env_commands_never_sends() {
        assert_eq!(class_of(&["ocx", "env"]), Class::Send);
        assert_eq!(class_of(&["ocx", "env", "--shell=bash"]), Class::Never);
        assert_eq!(class_of(&["ocx", "env", "--shell"]), Class::Never);
        assert_eq!(class_of(&["ocx", "package", "env", "cmake:3"]), Class::Send);
        assert_eq!(
            class_of(&["ocx", "package", "env", "--shell=bash", "cmake:3"]),
            Class::Never
        );
    }

    #[test]
    fn a_command_line_without_a_subcommand_or_with_a_plugin_never_sends() {
        assert_eq!(class_of(&["ocx"]), Class::Never);
        assert_eq!(class_of(&["ocx", "no-such-plugin", "arg"]), Class::Never);
        assert_eq!(class_of(&["ocx", "pull", "cmake:3"]), Class::Send);
    }

    #[test]
    fn exit_code_value_round_trips_every_u8() {
        for value in 0..=u8::MAX {
            assert_eq!(exit_code_value(ExitCode::from(value)), value, "exit code {value}");
        }
    }

    fn outcome_of(result: anyhow::Result<ExitCode>) -> CommandOutcome {
        command_outcome("pull".to_owned(), Duration::from_secs(2), &result)
    }

    #[test]
    fn an_ok_result_is_a_success_even_when_its_exit_code_is_not_zero() {
        let outcome = outcome_of(Ok(ExitCode::from(42)));
        assert_eq!(outcome.exit_code, 42);
        assert_eq!(outcome.result, CommandResult::Succeeded);
        assert_eq!(outcome.name, "pull");
        assert_eq!(outcome.duration, Duration::from_secs(2));
    }

    #[test]
    fn an_error_carries_the_exit_code_and_slug_the_process_boundary_uses() {
        let error = anyhow::Error::new(crate::app::CliRefusal::InvalidCommandLine("bad".to_owned()));
        let expected = crate::exit::classify_error(error.as_ref());
        let outcome = outcome_of(Err(error));
        assert_eq!(outcome.exit_code, 64, "a usage refusal exits 64");
        assert_eq!(outcome.exit_code, expected as u8);
        assert_eq!(
            outcome.result,
            CommandResult::Failed {
                detail: Some("invalid_command_line".to_owned())
            }
        );
    }

    #[test]
    fn an_unclassified_error_exits_one_with_no_slug() {
        let outcome = outcome_of(Err(anyhow::anyhow!("no classified cause")));
        assert_eq!(outcome.exit_code, 1);
        assert_eq!(outcome.result, CommandResult::Failed { detail: None });
    }

    #[test]
    fn the_command_name_is_the_leaf_path_so_direnv_leaves_stay_apart() {
        use clap::Parser as _;
        let name = |words: &[&str]| {
            let cli = crate::app::Cli::try_parse_from(words).expect("the fixture is a valid invocation");
            command_name(cli.command.as_ref().expect("a subcommand"))
        };
        assert_eq!(name(&["ocx", "package", "install", "cmake:3"]), "package install");
        assert_eq!(name(&["ocx", "direnv", "init"]), "direnv init");
        assert_eq!(name(&["ocx", "pull"]), "pull");
    }

    // ── Package records ──────────────────────────────────────────────────────

    const FETCHED_HEX: &str = "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0";

    fn pinned(repository: &str) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::pin(
            &ocx_oci::PackageRef::new_registry(repository, "index.ocx.sh").clone_with_tag("3"),
            ocx_oci::Digest::Sha256(FETCHED_HEX.to_owned()),
        )
    }

    fn activity(repository: &str, action: Action, store_hit: bool) -> PackageActivity {
        PackageActivity {
            pinned: pinned(repository),
            platform: None,
            action,
            role: Role::Root,
            store_hit,
            download_size: if store_hit { 0 } else { 4096 },
            resolve: Some(Duration::from_millis(5)),
            download: None,
            extract: None,
        }
    }

    /// The record's size follows the store: a fetch carries the bytes it pulled, a store hit and a
    /// removal carry none.
    #[test]
    fn download_size_is_present_only_for_a_fetch() {
        let fetched = package_record(activity("ocx/cmake", Action::Pulled, false)).expect("a record");
        assert_eq!(fetched.download_size, Some(4096));
        assert!(!fetched.store_hit);

        let hit = package_record(activity("ocx/cmake", Action::Installed, true)).expect("a record");
        assert_eq!(hit.download_size, None);
        assert!(hit.store_hit);

        let removed = package_record(activity("ocx/cmake", Action::Removed, false)).expect("a record");
        assert_eq!(removed.download_size, None, "a removal fetched nothing");
        assert_eq!(removed.action, PackageAction::Removed);
    }

    #[test]
    fn action_role_phases_and_purl_carry_over() {
        let mut entry = activity("ocx/cmake", Action::Installed, false);
        entry.role = Role::Dependency;
        entry.download = Some(Duration::from_millis(40));
        entry.extract = Some(Duration::from_millis(7));
        let expected_purl = package_url(&entry.pinned, None).expect("a logical identity");

        let record = package_record(entry).expect("a record");

        assert_eq!(record.purl, expected_purl);
        assert!(record.purl.starts_with("pkg:oci/cmake@sha256:"), "{}", record.purl);
        assert_eq!(record.action, PackageAction::Installed);
        assert_eq!(record.role, PackageRole::Dependency);
        assert_eq!(record.resolve, Some(Duration::from_millis(5)));
        assert_eq!(record.download, Some(Duration::from_millis(40)));
        assert_eq!(record.extract, Some(Duration::from_millis(7)));
    }

    #[test]
    fn an_entry_without_a_logical_identity_produces_no_record() {
        assert!(package_record(activity("file-url-mode/anonymous", Action::Pulled, false)).is_none());
    }

    #[test]
    fn package_records_drain_the_log_once_in_identifier_order() {
        let log = ActivityLog::new();
        log.record(activity("ocx/ninja", Action::Pulled, false));
        log.record(activity("ocx/cmake", Action::Pulled, false));

        let records = package_records(&log);

        let names: Vec<&str> = records
            .iter()
            .map(|record| record.purl.split('@').next().unwrap_or_default())
            .collect();
        assert_eq!(names, ["pkg:oci/cmake", "pkg:oci/ninja"]);
        assert!(package_records(&log).is_empty(), "the log is empty after a drain");
    }

    // ── Name parity with execution records ───────────────────────────────────

    /// A record that sets every field the shared facts live in.
    fn execution_record() -> ocx_package_manager::record::ExecutionRecord {
        use ocx_package_manager::record::execution_record::{
            EnvMode, FrameIdentity, Host, OcxBuild, Os, ParentProcess, Process, Resolution, ScopeBlock,
        };
        use ocx_package_manager::record::{ExecutionRecord, Frame, FrameCommand, ResourceDescriptor};
        use std::collections::BTreeMap;

        let package = pinned("ocx/cmake");
        ExecutionRecord {
            schema_version: "1".to_owned(),
            kind: "sh.ocx.execution-record".to_owned(),
            recorded_at: "2026-11-19T10:00:00.000Z".parse().expect("timestamp"),
            ocx: OcxBuild {
                version: "0.6.8".to_owned(),
                binary: "/home/ci/.ocx/bin/ocx".into(),
            },
            frame: Frame {
                command: FrameCommand::PackageExec,
                identity: FrameIdentity::Complete,
                identity_note: None,
            },
            process: Process {
                pid: 4242,
                parent: Some(ParentProcess { pid: 4141 }),
                user: None,
                arch: Some(ocx_oci::Architecture::Amd64),
                executable: "/home/ci/.ocx/packages/cmake/bin/cmake".into(),
                working_directory: None,
            },
            host: Host {
                name: Some("ci-host".to_owned()),
            },
            os: Os {
                os_type: Some(ocx_oci::OperatingSystem::Linux),
            },
            executable: BTreeMap::from([(
                "sh.ocx.package".to_owned(),
                package_url(&package, None).expect("a logical identity"),
            )]),
            env_mode: EnvMode::Layer,
            scope: ScopeBlock::Launcher,
            resolution: Resolution {
                offline: false,
                remote: false,
                frozen: false,
                patch_snapshot: BTreeMap::new(),
                no_verify: false,
                requested_platform: None,
                registries: None,
                insecure_registries: Vec::new(),
                mirrors: None,
                managed_config: None,
                auto_installed: None,
            },
            packages: vec![ResourceDescriptor {
                name: "cmake".to_owned(),
                uri: package_url(&package, None),
                digest: BTreeMap::from([("sha256".to_owned(), FETCHED_HEX.to_owned())]),
                annotations: BTreeMap::from([("sh.ocx.role".to_owned(), "root".into())]),
            }],
        }
    }

    /// Every key path of `value`, `/`-separated, with `[]` for a step through an array.
    fn key_paths(value: &serde_json::Value, prefix: &str, paths: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, nested) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}/{key}")
                    };
                    key_paths(nested, &path, paths);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    key_paths(item, &format!("{prefix}[]"), paths);
                }
            }
            _ => {
                paths.insert(prefix.to_owned());
            }
        }
    }

    /// A consumer joins a usage record to an execution record on these names, so they are one
    /// vocabulary. The OpenTelemetry resource keys nest as dotted segments in the execution record
    /// (`process.parent.pid` is `process/parent/pid`); the `sh.ocx.*` keys are single keys, found
    /// beneath whichever block carries them.
    ///
    /// Red state: rename `host.name` in the telemetry resource without renaming it here, or the
    /// execution record's `host.name` field, and its row below fails.
    #[test]
    fn telemetry_names_match_execution_record_keys() {
        let value = serde_json::to_value(execution_record()).expect("an execution record serializes");
        let mut paths = std::collections::BTreeSet::new();
        key_paths(&value, "", &mut paths);

        for key in ocx_telemetry::keys::RESOURCE_JOIN_KEYS {
            let path = key.replace('.', "/");
            assert!(paths.contains(&path), "`{key}` is not `{path}` in {paths:?}");
        }
        for key in ocx_telemetry::keys::RECORD_JOIN_KEYS {
            assert!(
                paths.iter().any(|path| path.rsplit('/').next() == Some(key)),
                "`{key}` is not a key of the execution record: {paths:?}"
            );
        }
    }
}
