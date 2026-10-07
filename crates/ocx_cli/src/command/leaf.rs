// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Every command leaf `ocx` runs, with its path and output contract.
//!
//! [`Leaf`] is the one place a leaf's name is written: the `--format json` contract table, the
//! canonical command name in the error document and the deprecated-spelling rows all read it, and
//! [`Command::leaf`] matches the clap tree exhaustively, so a command without a row does not compile.

use super::Command;
use super::contract::OutputMode::{self, Empty, Passthrough, RawDocument, ShellStream};
use crate::api::data;

/// Declares [`Leaf`] and `Leaf::ALL` from one list, so a leaf cannot exist without being listed.
macro_rules! leaves {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        /// One runnable command: a path below `ocx` that no further subcommand narrows.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Leaf {
            $($(#[$doc])* $name,)*
        }

        impl Leaf {
            /// Every leaf, in declaration order.
            pub const ALL: &[Leaf] = &[$(Leaf::$name,)*];
        }
    };
}

leaves! {
    About,
    Add,
    Clean,
    ConfigPush,
    ConfigSetup,
    ConfigTest,
    ConfigUpdate,
    DirenvExport,
    DirenvInit,
    Env,
    Exec,
    IndexCatalog,
    IndexList,
    IndexRegenerate,
    IndexSync,
    IndexUpdate,
    Init,
    Inspect,
    LauncherExec,
    LauncherShim,
    Lock,
    Login,
    Logout,
    PackageAnnounce,
    PackageAttest,
    PackageCascadeCheck,
    PackageCascadeRepair,
    PackageClaim,
    PackageCopy,
    PackageCreate,
    PackageDeps,
    /// Deprecated spelling `ocx package describe`; keeps its released contract row until it is removed.
    PackageDescribe,
    PackageDescriptionPull,
    PackageDescriptionPush,
    PackageDeselect,
    PackageEnv,
    PackageExec,
    /// Deprecated spelling `ocx package info`; keeps its released contract row until it is removed.
    PackageInfo,
    PackageInspect,
    PackageInstall,
    PackagePrune,
    PackagePull,
    PackagePush,
    PackageReceipt,
    PackageSbom,
    PackageSelect,
    PackageSign,
    PackageTest,
    PackageUninstall,
    PackageVerify,
    PackageWhich,
    PatchFreeze,
    PatchPublish,
    PatchSync,
    PatchTest,
    PatchWhy,
    Pull,
    Remove,
    /// Deprecated spelling `ocx run`; keeps its released contract row until it is removed.
    Run,
    SelfActivate,
    SelfSetup,
    SelfUpdate,
    ShellAllow,
    ShellCompletion,
    ShellRevoke,
    ShellState,
    Status,
    Update,
    Upgrade,
    Version,
}

impl Leaf {
    /// The words after `ocx` that select this command.
    pub const fn path(self) -> &'static [&'static str] {
        match self {
            Self::About => &["about"],
            Self::Add => &["add"],
            Self::Clean => &["clean"],
            Self::ConfigPush => &["config", "push"],
            Self::ConfigSetup => &["config", "setup"],
            Self::ConfigTest => &["config", "test"],
            Self::ConfigUpdate => &["config", "update"],
            Self::DirenvExport => &["direnv", "export"],
            Self::DirenvInit => &["direnv", "init"],
            Self::Env => &["env"],
            Self::Exec => &["exec"],
            Self::IndexCatalog => &["index", "catalog"],
            Self::IndexList => &["index", "list"],
            Self::IndexRegenerate => &["index", "regenerate"],
            Self::IndexSync => &["index", "sync"],
            Self::IndexUpdate => &["index", "update"],
            Self::Init => &["init"],
            Self::Inspect => &["inspect"],
            Self::LauncherExec => &["launcher", "exec"],
            Self::LauncherShim => &["launcher", "shim"],
            Self::Lock => &["lock"],
            Self::Login => &["login"],
            Self::Logout => &["logout"],
            Self::PackageAnnounce => &["package", "announce"],
            Self::PackageAttest => &["package", "attest"],
            Self::PackageCascadeCheck => &["package", "cascade", "check"],
            Self::PackageCascadeRepair => &["package", "cascade", "repair"],
            Self::PackageClaim => &["package", "claim"],
            Self::PackageCopy => &["package", "copy"],
            Self::PackageCreate => &["package", "create"],
            Self::PackageDeps => &["package", "deps"],
            Self::PackageDescribe => &["package", "describe"],
            Self::PackageDescriptionPull => &["package", "description", "pull"],
            Self::PackageDescriptionPush => &["package", "description", "push"],
            Self::PackageDeselect => &["package", "deselect"],
            Self::PackageEnv => &["package", "env"],
            Self::PackageExec => &["package", "exec"],
            Self::PackageInfo => &["package", "info"],
            Self::PackageInspect => &["package", "inspect"],
            Self::PackageInstall => &["package", "install"],
            Self::PackagePrune => &["package", "prune"],
            Self::PackagePull => &["package", "pull"],
            Self::PackagePush => &["package", "push"],
            Self::PackageReceipt => &["package", "receipt"],
            Self::PackageSbom => &["package", "sbom"],
            Self::PackageSelect => &["package", "select"],
            Self::PackageSign => &["package", "sign"],
            Self::PackageTest => &["package", "test"],
            Self::PackageUninstall => &["package", "uninstall"],
            Self::PackageVerify => &["package", "verify"],
            Self::PackageWhich => &["package", "which"],
            Self::PatchFreeze => &["patch", "freeze"],
            Self::PatchPublish => &["patch", "publish"],
            Self::PatchSync => &["patch", "sync"],
            Self::PatchTest => &["patch", "test"],
            Self::PatchWhy => &["patch", "why"],
            Self::Pull => &["pull"],
            Self::Remove => &["remove"],
            Self::Run => &["run"],
            Self::SelfActivate => &["self", "activate"],
            Self::SelfSetup => &["self", "setup"],
            Self::SelfUpdate => &["self", "update"],
            Self::ShellAllow => &["shell", "allow"],
            Self::ShellCompletion => &["shell", "completion"],
            Self::ShellRevoke => &["shell", "revoke"],
            Self::ShellState => &["shell", "state"],
            Self::Status => &["status"],
            Self::Update => &["update"],
            Self::Upgrade => &["upgrade"],
            Self::Version => &["version"],
        }
    }

    /// The contract version and the stdout shapes `--format json` can produce.
    pub const fn contract(self) -> (u32, &'static [OutputMode]) {
        match self {
            Self::About => (1, const { &[OutputMode::report::<data::about::About>()] }),
            Self::Add => (1, const { &[OutputMode::report::<data::lock::LockReport>()] }),
            Self::Clean => (1, const { &[OutputMode::report::<data::clean::Clean>()] }),
            Self::ConfigPush => (1, const { &[OutputMode::report::<data::push::PushReport>()] }),
            Self::ConfigSetup => (
                1,
                const { &[OutputMode::report::<data::config_setup::ConfigSetupData>()] },
            ),
            Self::ConfigTest => (
                1,
                const { &[OutputMode::report::<data::config_test::ConfigTestData>()] },
            ),
            Self::ConfigUpdate => (
                1,
                const { &[OutputMode::report::<data::config_update::ConfigUpdateData>()] },
            ),
            Self::DirenvExport => (1, &[ShellStream]),
            Self::DirenvInit => (1, &[Empty]),
            Self::Env => (1, const { &[OutputMode::report::<data::env::EnvVars>(), ShellStream] }),
            Self::Exec => (1, &[Passthrough]),
            Self::IndexCatalog => (1, const { &[OutputMode::report::<data::catalog::Catalog>()] }),
            Self::IndexList => (1, const { &[OutputMode::report::<data::tag::Tags>()] }),
            Self::IndexRegenerate => (1, const { &[OutputMode::report::<data::index::RegenerateReport>()] }),
            Self::IndexSync => (
                1,
                const { &[OutputMode::report::<data::index::CatalogPreview>(), Empty] },
            ),
            Self::IndexUpdate => (1, &[Empty]),
            Self::Init => (1, &[Empty]),
            Self::Inspect => (
                1,
                const {
                    &[
                        OutputMode::report::<data::package_inspect::InspectReport>(),
                        OutputMode::report_then_fail::<data::package_inspect::InspectReport>(),
                    ]
                },
            ),
            Self::LauncherExec => (1, &[Passthrough]),
            Self::LauncherShim => (1, &[Passthrough]),
            Self::Lock => (1, const { &[OutputMode::report::<data::lock::LockReport>()] }),
            Self::Login => (1, const { &[OutputMode::report::<data::login::LoginResult>()] }),
            Self::Logout => (1, const { &[OutputMode::report::<data::login::LogoutResult>()] }),
            Self::PackageAnnounce => (1, const { &[OutputMode::report::<data::announce::AnnounceReport>()] }),
            Self::PackageAttest => (
                1,
                const {
                    &[
                        OutputMode::report::<data::attestation::AttestationReport>(),
                        OutputMode::report::<data::sweep::SweepReport<data::attestation::AttestationReport>>(),
                        OutputMode::report_then_fail::<data::sweep::SweepReport<data::attestation::AttestationReport>>(
                        ),
                    ]
                },
            ),
            Self::PackageCascadeCheck => (
                1,
                const {
                    &[
                        OutputMode::report::<data::package_cascade_check::PackageCascadeCheck>(),
                        OutputMode::report_then_fail::<data::package_cascade_check::PackageCascadeCheck>(),
                    ]
                },
            ),
            Self::PackageCascadeRepair => (
                1,
                const {
                    &[
                        OutputMode::report::<data::package_cascade_repair::PackageCascadeRepair>(),
                        OutputMode::report_then_fail::<data::package_cascade_repair::PackageCascadeRepair>(),
                    ]
                },
            ),
            Self::PackageClaim => (1, const { &[OutputMode::report::<data::claim::ClaimReport>()] }),
            Self::PackageCopy => (
                1,
                const {
                    &[
                        OutputMode::report::<data::package_copy::CopyReport>(),
                        OutputMode::report_then_fail::<data::package_copy::CopyReport>(),
                    ]
                },
            ),
            Self::PackageCreate => (1, &[Empty]),
            Self::PackageDeps => (
                1,
                const {
                    &[
                        OutputMode::report::<data::deps::Dependencies>(),
                        OutputMode::report::<data::deps::DependenciesTrace>(),
                        OutputMode::report_then_fail::<data::deps::DependenciesTrace>(),
                        OutputMode::report::<data::deps::FlatDependencies>(),
                    ]
                },
            ),
            Self::PackageDescribe => (1, &[Empty]),
            Self::PackageDescriptionPull => (
                1,
                const { &[OutputMode::report::<data::package_description::PackageDescriptions>()] },
            ),
            Self::PackageDescriptionPush => (1, &[Empty]),
            Self::PackageDeselect => (1, const { &[OutputMode::report::<data::removed::Removed>()] }),
            Self::PackageEnv => (1, const { &[OutputMode::report::<data::env::EnvVars>(), ShellStream] }),
            Self::PackageExec => (1, &[Passthrough]),
            Self::PackageInfo => (
                1,
                const { &[OutputMode::report::<data::package_description::PackageDescriptions>()] },
            ),
            Self::PackageInspect => (
                1,
                const {
                    &[
                        OutputMode::report::<data::package_inspect::InspectReport>(),
                        OutputMode::report_then_fail::<data::package_inspect::InspectReport>(),
                    ]
                },
            ),
            Self::PackageInstall => (1, const { &[OutputMode::report::<data::install::Installs>()] }),
            Self::PackagePrune => (
                1,
                const {
                    &[
                        OutputMode::report::<data::package_prune::PackagePrune>(),
                        OutputMode::report_then_fail::<data::package_prune::PackagePrune>(),
                    ]
                },
            ),
            Self::PackagePull => (1, const { &[OutputMode::report::<data::paths::Paths>()] }),
            Self::PackagePush => (
                1,
                const {
                    &[
                        OutputMode::report::<data::push::PushReport>(),
                        OutputMode::report_then_fail::<data::push::PushReport>(),
                    ]
                },
            ),
            Self::PackageReceipt => (
                1,
                const { &[OutputMode::report::<data::package_receipt::PackageReceipt>()] },
            ),
            Self::PackageSbom => (
                1,
                const {
                    &[
                        OutputMode::report::<data::sbom::SbomListingReport>(),
                        Empty,
                        RawDocument,
                    ]
                },
            ),
            Self::PackageSelect => (1, const { &[OutputMode::report::<data::install::Installs>()] }),
            Self::PackageSign => (
                1,
                const {
                    &[
                        OutputMode::report::<data::signature::SignatureReport>(),
                        OutputMode::report_then_fail::<data::signature::SignatureReport>(),
                        OutputMode::report::<data::sweep::SweepReport<data::signature::SignatureReport>>(),
                        OutputMode::report_then_fail::<data::sweep::SweepReport<data::signature::SignatureReport>>(),
                    ]
                },
            ),
            Self::PackageTest => (
                1,
                const {
                    &[
                        Passthrough,
                        OutputMode::report::<data::script_run::ScriptRunReport>(),
                        OutputMode::report_then_fail::<data::script_run::ScriptRunReport>(),
                    ]
                },
            ),
            Self::PackageUninstall => (1, const { &[OutputMode::report::<data::removed::Removed>()] }),
            Self::PackageVerify => (
                1,
                const { &[OutputMode::report::<data::verification::VerificationReport>()] },
            ),
            Self::PackageWhich => (1, const { &[OutputMode::report::<data::paths::LocatedPaths>()] }),
            Self::PatchFreeze => (
                1,
                const { &[OutputMode::report::<data::patch_freeze::PatchFreezeReport>()] },
            ),
            Self::PatchPublish => (
                1,
                const { &[OutputMode::report::<data::patch_publish::PatchPublishReport>()] },
            ),
            Self::PatchSync => (
                1,
                const { &[OutputMode::report::<data::patch_sync::PatchSyncReport>()] },
            ),
            Self::PatchTest => (
                1,
                const {
                    &[
                        OutputMode::report::<data::patch_test::PatchTestReport>(),
                        Passthrough,
                        OutputMode::report::<data::script_run::ScriptRunReport>(),
                        OutputMode::report_then_fail::<data::script_run::ScriptRunReport>(),
                    ]
                },
            ),
            Self::PatchWhy => (1, const { &[OutputMode::report::<data::patch_why::PatchWhyReport>()] }),
            Self::Pull => (
                1,
                const {
                    &[
                        OutputMode::report::<data::warmed_paths::WarmedPaths>(),
                        OutputMode::report::<data::pull_dry_run::PullDryRun>(),
                    ]
                },
            ),
            Self::Remove => (1, const { &[OutputMode::report::<data::lock::LockReport>()] }),
            Self::Run => (1, &[Passthrough]),
            Self::SelfActivate => (1, &[ShellStream]),
            Self::SelfSetup => (1, const { &[OutputMode::report::<data::self_setup::SelfSetupData>()] }),
            Self::SelfUpdate => (
                1,
                const {
                    &[
                        OutputMode::report::<data::self_update::SelfUpdateData>(),
                        OutputMode::report::<data::self_update::UpdateCheckData>(),
                    ]
                },
            ),
            Self::ShellAllow => (1, &[Empty]),
            Self::ShellCompletion => (1, &[ShellStream]),
            Self::ShellRevoke => (1, &[Empty]),
            Self::ShellState => (
                1,
                const {
                    &[
                        OutputMode::report::<data::shell_state::ShellStateReport>(),
                        OutputMode::report::<data::shell_state::VerboseShellState>(),
                    ]
                },
            ),
            Self::Status => (1, const { &[OutputMode::report::<data::status::StatusReport>()] }),
            Self::Update => (
                1,
                const {
                    &[
                        OutputMode::report::<data::update::UpdateReport>(),
                        OutputMode::report::<data::update::VerboseUpdateReport>(),
                        OutputMode::report_then_fail::<data::update::UpdateReport>(),
                        OutputMode::report_then_fail::<data::update::VerboseUpdateReport>(),
                    ]
                },
            ),
            Self::Upgrade => (
                1,
                const {
                    &[
                        OutputMode::report::<data::upgrade::UpgradeReport>(),
                        OutputMode::report::<data::upgrade::VerboseUpgradeReport>(),
                        OutputMode::report_then_fail::<data::upgrade::UpgradeReport>(),
                        OutputMode::report_then_fail::<data::upgrade::VerboseUpgradeReport>(),
                    ]
                },
            ),
            Self::Version => (
                1,
                const {
                    &[
                        OutputMode::report::<data::version::VersionData>(),
                        OutputMode::report::<data::version::VerboseVersionData>(),
                    ]
                },
            ),
        }
    }
}

impl Command {
    /// The leaf this invocation runs; `None` for an external `ocx-<name>` plugin, which has no row.
    pub fn leaf(&self) -> Option<Leaf> {
        use super::config::ConfigGroup;
        use super::index::Index;
        use super::launcher::Launcher;
        use super::package::Package;
        use super::package_cascade::CascadeGroup;
        use super::package_description::DescriptionGroup;
        use super::patch::PatchGroup;
        use super::self_group::SelfGroup;
        use super::shell::Shell;

        Some(match self {
            Command::Env(_) => Leaf::Env,
            Command::Add(_) => Leaf::Add,
            Command::Clean(_) => Leaf::Clean,
            Command::Config(group) => match group {
                ConfigGroup::Setup(_) => Leaf::ConfigSetup,
                ConfigGroup::Update(_) => Leaf::ConfigUpdate,
                ConfigGroup::Push(_) => Leaf::ConfigPush,
                ConfigGroup::Test(_) => Leaf::ConfigTest,
            },
            Command::Direnv(direnv) => direnv.leaf(),
            Command::Index(group) => match group {
                Index::Catalog(_) => Leaf::IndexCatalog,
                Index::List(_) => Leaf::IndexList,
                Index::Update(_) => Leaf::IndexUpdate,
                Index::Sync(_) => Leaf::IndexSync,
                Index::Regenerate(_) => Leaf::IndexRegenerate,
            },
            Command::About(_) => Leaf::About,
            Command::Init(_) => Leaf::Init,
            Command::Inspect(_) => Leaf::Inspect,
            Command::Status(_) => Leaf::Status,
            Command::Lock(_) => Leaf::Lock,
            Command::Login(_) => Leaf::Login,
            Command::Logout(_) => Leaf::Logout,
            Command::Update(_) => Leaf::Update,
            Command::Upgrade(_) => Leaf::Upgrade,
            Command::Launcher(group) => match group {
                Launcher::Exec(_) => Leaf::LauncherExec,
                Launcher::Shim(_) => Leaf::LauncherShim,
            },
            Command::Package(group) => match group {
                Package::Announce(_) => Leaf::PackageAnnounce,
                Package::Attest(_) => Leaf::PackageAttest,
                Package::Cascade(group) => match group {
                    CascadeGroup::Check(_) => Leaf::PackageCascadeCheck,
                    CascadeGroup::Repair(_) => Leaf::PackageCascadeRepair,
                },
                Package::Claim(_) => Leaf::PackageClaim,
                Package::Copy(_) => Leaf::PackageCopy,
                Package::Create(_) => Leaf::PackageCreate,
                Package::Description(group) => match group {
                    DescriptionGroup::Push(_) => Leaf::PackageDescriptionPush,
                    DescriptionGroup::Pull(_) => Leaf::PackageDescriptionPull,
                },
                Package::DeprecatedDescribe(_) => Leaf::PackageDescribe,
                Package::DeprecatedInfo(_) => Leaf::PackageInfo,
                Package::Deps(_) => Leaf::PackageDeps,
                Package::Env(_) => Leaf::PackageEnv,
                Package::Inspect(_) => Leaf::PackageInspect,
                Package::Prune(_) => Leaf::PackagePrune,
                Package::Install(_) => Leaf::PackageInstall,
                Package::Pull(_) => Leaf::PackagePull,
                Package::Push(_) => Leaf::PackagePush,
                Package::Receipt(_) => Leaf::PackageReceipt,
                Package::Sbom(_) => Leaf::PackageSbom,
                Package::Select(_) => Leaf::PackageSelect,
                Package::Deselect(_) => Leaf::PackageDeselect,
                Package::Sign(_) => Leaf::PackageSign,
                Package::Test(_) => Leaf::PackageTest,
                Package::Verify(_) => Leaf::PackageVerify,
                Package::Exec(_) => Leaf::PackageExec,
                Package::Uninstall(_) => Leaf::PackageUninstall,
                Package::Which(_) => Leaf::PackageWhich,
            },
            Command::Patch(group) => match group {
                PatchGroup::Freeze(_) => Leaf::PatchFreeze,
                PatchGroup::Sync(_) => Leaf::PatchSync,
                PatchGroup::Publish(_) => Leaf::PatchPublish,
                PatchGroup::Test(_) => Leaf::PatchTest,
                PatchGroup::Why(_) => Leaf::PatchWhy,
            },
            Command::Pull(_) => Leaf::Pull,
            Command::Remove(_) => Leaf::Remove,
            Command::Exec(_) => Leaf::Exec,
            Command::DeprecatedRun(_) => Leaf::Run,
            Command::Shell(group) => match group {
                Shell::Allow(_) => Leaf::ShellAllow,
                Shell::Completion(_) => Leaf::ShellCompletion,
                Shell::Revoke(_) => Leaf::ShellRevoke,
                Shell::State(_) => Leaf::ShellState,
            },
            Command::Self_(group) => match group {
                SelfGroup::Activate(_) => Leaf::SelfActivate,
                SelfGroup::Setup(_) => Leaf::SelfSetup,
                SelfGroup::Update(_) => Leaf::SelfUpdate,
            },
            Command::Version(_) => Leaf::Version,
            Command::External(_) => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::{FromArgMatches as _, Subcommand as _};

    use super::*;

    fn parse(words: &[&str]) -> Command {
        let matches = Command::augment_subcommands(clap::Command::new("ocx"))
            .try_get_matches_from(words)
            .expect("the fixture is a valid invocation");
        Command::from_arg_matches(&matches).expect("clap built the matches from this tree")
    }

    #[test]
    fn leaf_paths_are_unique() {
        for (index, leaf) in Leaf::ALL.iter().enumerate() {
            assert!(
                !Leaf::ALL[..index].iter().any(|other| other.path() == leaf.path()),
                "{leaf:?} repeats the path `ocx {}`",
                leaf.path().join(" ")
            );
        }
    }

    /// The contract lint covers visible leaves only; this one also holds the hidden ones.
    #[test]
    fn every_leaf_is_a_command_with_no_subcommand_in_the_live_tree() {
        let root = Command::augment_subcommands(clap::Command::new("ocx"));
        for leaf in Leaf::ALL {
            let mut command = &root;
            for word in leaf.path() {
                command = command
                    .find_subcommand(word)
                    .unwrap_or_else(|| panic!("`ocx {}` is not in the clap tree", leaf.path().join(" ")));
            }
            assert!(
                !command.has_subcommands(),
                "`ocx {}` is a group, not a leaf",
                leaf.path().join(" ")
            );
        }
    }

    #[test]
    fn a_parsed_invocation_reports_the_leaf_it_runs() {
        let rows: [(&[&str], Option<Leaf>); 6] = [
            (
                &["ocx", "package", "cascade", "check", "cmake"],
                Some(Leaf::PackageCascadeCheck),
            ),
            (&["ocx", "direnv", "export"], Some(Leaf::DirenvExport)),
            (&["ocx", "direnv"], Some(Leaf::DirenvInit)),
            (&["ocx", "run", "--", "true"], Some(Leaf::Run)),
            (&["ocx", "package", "describe", "cmake"], Some(Leaf::PackageDescribe)),
            (&["ocx", "ocx-plugin"], None),
        ];
        for (words, expected) in rows {
            assert_eq!(parse(words).leaf(), expected, "`{}`", words.join(" "));
        }
    }
}
