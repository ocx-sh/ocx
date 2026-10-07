// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The contract versions this SDK was generated for, and decoding by root name.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::runtime::{Error, check_version};
use super::types::{
    About, AnnounceReport, AttestationReport, AttestationReportSweep, Catalog, CatalogPreview, ClaimReport, Clean,
    ConfigSetupData, ConfigTestData, ConfigUpdateData, CopyReport, DependenciesTrace, DependencyTree, EnvVars,
    ErrorDocument, FlatDependencies, InspectReport, Installs, LocatedPaths, LockReport, LoginResult, LogoutResult,
    PackageCascadeCheck, PackageCascadeRepair, PackageDescriptions, PackageReceipt, PatchFreezeReport,
    PatchPublishReport, PatchSyncReport, PatchTestReport, PatchWhyReport, Paths, PruneOutcome, PullDryRun, PushReport,
    RegenerateReport, Removed, SbomListingReport, ScriptRunReport, SelfSetupData, SelfUpdateData, ShellStateReport,
    SignatureReport, SignatureReportSweep, StatusReport, Tags, UpdateCheckData, UpdateReport, UpgradeReport,
    VerboseShellState, VerboseUpdateReport, VerboseUpgradeReport, VerboseVersionData, VerificationReport, VersionData,
    WarmedPaths,
};
use super::wire::Unknowns;

/// The oldest `ocx` release known to publish the contract.
pub const MINIMUM_OCX: &str = "0.6.4";

/// The `schema_version` of the error document.
pub const ERRORS: u32 = 2;

/// Each command's contract version, keyed by its words below `ocx`, sorted.
pub const COMMANDS: &[(&str, u32)] = &[
    ("about", 1),
    ("add", 1),
    ("clean", 1),
    ("config push", 1),
    ("config setup", 1),
    ("config test", 1),
    ("config update", 1),
    ("direnv export", 1),
    ("direnv init", 1),
    ("env", 1),
    ("exec", 1),
    ("index catalog", 1),
    ("index list", 1),
    ("index regenerate", 1),
    ("index sync", 1),
    ("index update", 1),
    ("init", 1),
    ("inspect", 1),
    ("launcher exec", 1),
    ("launcher shim", 1),
    ("lock", 1),
    ("login", 1),
    ("logout", 1),
    ("package announce", 1),
    ("package attest", 1),
    ("package cascade check", 1),
    ("package cascade repair", 1),
    ("package claim", 1),
    ("package copy", 1),
    ("package create", 1),
    ("package deps", 1),
    ("package description pull", 1),
    ("package description push", 1),
    ("package deselect", 1),
    ("package env", 1),
    ("package exec", 1),
    ("package inspect", 1),
    ("package install", 1),
    ("package prune", 1),
    ("package pull", 1),
    ("package push", 1),
    ("package receipt", 1),
    ("package sbom", 1),
    ("package select", 1),
    ("package sign", 1),
    ("package test", 1),
    ("package uninstall", 1),
    ("package verify", 1),
    ("package which", 1),
    ("patch freeze", 1),
    ("patch publish", 1),
    ("patch sync", 1),
    ("patch test", 1),
    ("patch why", 1),
    ("pull", 1),
    ("remove", 1),
    ("self activate", 1),
    ("self setup", 1),
    ("self update", 1),
    ("shell allow", 1),
    ("shell completion", 1),
    ("shell revoke", 1),
    ("shell state", 1),
    ("status", 1),
    ("update", 1),
    ("upgrade", 1),
    ("version", 1),
];

/// Each report root's `schema_version`, keyed by the root name the command grammar uses, sorted.
pub const REPORTS: &[(&str, u32)] = &[
    ("About", 1),
    ("AnnounceReport", 1),
    ("AttestationReport", 2),
    ("Catalog", 1),
    ("CatalogPreview", 1),
    ("ClaimReport", 1),
    ("Clean", 1),
    ("ConfigSetupData", 1),
    ("ConfigTestData", 1),
    ("ConfigUpdateData", 1),
    ("CopyReport", 1),
    ("Dependencies", 1),
    ("DependenciesTrace", 1),
    ("EnvVars", 1),
    ("FlatDependencies", 1),
    ("InspectReport", 1),
    ("Installs", 1),
    ("LocatedPaths", 1),
    ("LockReport", 1),
    ("LoginResult", 1),
    ("LogoutResult", 1),
    ("PackageCascadeCheck", 1),
    ("PackageCascadeRepair", 1),
    ("PackageDescriptions", 1),
    ("PackagePrune", 1),
    ("PackageReceipt", 1),
    ("PatchFreezeReport", 1),
    ("PatchPublishReport", 1),
    ("PatchSyncReport", 1),
    ("PatchTestReport", 1),
    ("PatchWhyReport", 1),
    ("Paths", 1),
    ("PullDryRun", 1),
    ("PushReport", 1),
    ("RegenerateReport", 1),
    ("Removed", 1),
    ("SbomListingReport", 2),
    ("ScriptRunReport", 1),
    ("SelfSetupData", 1),
    ("SelfUpdateData", 1),
    ("ShellStateReport", 1),
    ("SignatureReport", 2),
    ("StatusReport", 1),
    ("SweepReport<AttestationReport>", 2),
    ("SweepReport<SignatureReport>", 2),
    ("Tags", 1),
    ("UpdateCheckData", 1),
    ("UpdateReport", 1),
    ("UpgradeReport", 1),
    ("VerboseShellState", 1),
    ("VerboseUpdateReport", 1),
    ("VerboseUpgradeReport", 1),
    ("VerboseVersionData", 1),
    ("VerificationReport", 2),
    ("VersionData", 1),
    ("WarmedPaths", 1),
];

/// The version of command `command`; 0 for one this SDK does not know, which no binary reports.
pub fn command_version(command: &str) -> u32 {
    lookup(COMMANDS, command)
}

/// The `schema_version` of report root `root`; 0 for one this SDK does not know.
pub fn report_version(root: &str) -> u32 {
    lookup(REPORTS, root)
}

fn lookup(table: &[(&str, u32)], key: &str) -> u32 {
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map_or(0, |(_, version)| *version)
}

/// A decoded report root, for callers that pick the root by name at run time.
pub trait Decoded {
    /// The report as JSON again: every unset optional field absent, every enum value as sent.
    fn to_value(&self) -> Result<Value, serde_json::Error>;

    /// The JSON pointers of every value that landed in an `Unknown` arm.
    fn unknowns(&self) -> Vec<String>;
}

impl<T: Serialize + Unknowns> Decoded for T {
    fn to_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    fn unknowns(&self) -> Vec<String> {
        self.unknown_pointers()
    }
}

/// Decodes `document` as the report root `root`, after its `schema_version` matches; `None` for a root the
/// contract does not publish.
///
/// # Errors
///
/// [`Error::ContractMismatch`] on another `schema_version`, [`Error::Decode`] when the document is not that root.
pub fn decode_root(root: &str, document: &Value) -> Option<Result<Box<dyn Decoded>, Error>> {
    Some(match root {
        "About" => decode::<About>(root, document),
        "AnnounceReport" => decode::<AnnounceReport>(root, document),
        "AttestationReport" => decode::<AttestationReport>(root, document),
        "Catalog" => decode::<Catalog>(root, document),
        "CatalogPreview" => decode::<CatalogPreview>(root, document),
        "ClaimReport" => decode::<ClaimReport>(root, document),
        "Clean" => decode::<Clean>(root, document),
        "ConfigSetupData" => decode::<ConfigSetupData>(root, document),
        "ConfigTestData" => decode::<ConfigTestData>(root, document),
        "ConfigUpdateData" => decode::<ConfigUpdateData>(root, document),
        "CopyReport" => decode::<CopyReport>(root, document),
        "Dependencies" => decode::<DependencyTree>(root, document),
        "DependenciesTrace" => decode::<DependenciesTrace>(root, document),
        "EnvVars" => decode::<EnvVars>(root, document),
        "ErrorDocument" => decode::<ErrorDocument>(root, document),
        "FlatDependencies" => decode::<FlatDependencies>(root, document),
        "InspectReport" => decode::<InspectReport>(root, document),
        "Installs" => decode::<Installs>(root, document),
        "LocatedPaths" => decode::<LocatedPaths>(root, document),
        "LockReport" => decode::<LockReport>(root, document),
        "LoginResult" => decode::<LoginResult>(root, document),
        "LogoutResult" => decode::<LogoutResult>(root, document),
        "PackageCascadeCheck" => decode::<PackageCascadeCheck>(root, document),
        "PackageCascadeRepair" => decode::<PackageCascadeRepair>(root, document),
        "PackageDescriptions" => decode::<PackageDescriptions>(root, document),
        "PackagePrune" => decode::<PruneOutcome>(root, document),
        "PackageReceipt" => decode::<PackageReceipt>(root, document),
        "PatchFreezeReport" => decode::<PatchFreezeReport>(root, document),
        "PatchPublishReport" => decode::<PatchPublishReport>(root, document),
        "PatchSyncReport" => decode::<PatchSyncReport>(root, document),
        "PatchTestReport" => decode::<PatchTestReport>(root, document),
        "PatchWhyReport" => decode::<PatchWhyReport>(root, document),
        "Paths" => decode::<Paths>(root, document),
        "PullDryRun" => decode::<PullDryRun>(root, document),
        "PushReport" => decode::<PushReport>(root, document),
        "RegenerateReport" => decode::<RegenerateReport>(root, document),
        "Removed" => decode::<Removed>(root, document),
        "SbomListingReport" => decode::<SbomListingReport>(root, document),
        "ScriptRunReport" => decode::<ScriptRunReport>(root, document),
        "SelfSetupData" => decode::<SelfSetupData>(root, document),
        "SelfUpdateData" => decode::<SelfUpdateData>(root, document),
        "ShellStateReport" => decode::<ShellStateReport>(root, document),
        "SignatureReport" => decode::<SignatureReport>(root, document),
        "StatusReport" => decode::<StatusReport>(root, document),
        "SweepReport<AttestationReport>" => decode::<AttestationReportSweep>(root, document),
        "SweepReport<SignatureReport>" => decode::<SignatureReportSweep>(root, document),
        "Tags" => decode::<Tags>(root, document),
        "UpdateCheckData" => decode::<UpdateCheckData>(root, document),
        "UpdateReport" => decode::<UpdateReport>(root, document),
        "UpgradeReport" => decode::<UpgradeReport>(root, document),
        "VerboseShellState" => decode::<VerboseShellState>(root, document),
        "VerboseUpdateReport" => decode::<VerboseUpdateReport>(root, document),
        "VerboseUpgradeReport" => decode::<VerboseUpgradeReport>(root, document),
        "VerboseVersionData" => decode::<VerboseVersionData>(root, document),
        "VerificationReport" => decode::<VerificationReport>(root, document),
        "VersionData" => decode::<VersionData>(root, document),
        "WarmedPaths" => decode::<WarmedPaths>(root, document),
        _ => return None,
    })
}

fn decode<T>(root: &str, document: &Value) -> Result<Box<dyn Decoded>, Error>
where
    T: DeserializeOwned + Serialize + Unknowns + 'static,
{
    let expected = if root == "ErrorDocument" {
        ERRORS
    } else {
        report_version(root)
    };
    check_version(&format!("report `{root}`"), expected, document)?;
    let report = T::deserialize(document).map_err(Error::Decode)?;
    Ok(Box::new(report))
}
