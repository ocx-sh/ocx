// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_console::DataInterface;

use crate::options;

pub mod data;
pub mod junit;

/// An API data type that renders itself as plain text or, through [`Api::report`], as a versioned JSON root.
///
/// ```
/// # use ocx::api::Printable;
/// #[derive(serde::Serialize)]
/// struct Listing {
///     items: Vec<String>,
/// }
/// impl Printable for Listing {
///     const SCHEMA_VERSION: u32 = 1;
///     const ROOT: &'static str = "Listing";
///     fn print_plain(&self, _: &ocx_console::DataInterface) {}
/// }
/// ```
///
/// A root without its version does not compile:
///
/// ```compile_fail,E0046
/// # use ocx::api::Printable;
/// #[derive(serde::Serialize)]
/// struct Listing {
///     items: Vec<String>,
/// }
/// impl Printable for Listing {
///     const ROOT: &'static str = "Listing";
///     fn print_plain(&self, _: &ocx_console::DataInterface) {}
/// }
/// ```
///
/// Neither does one without its root name, which `cli.json` output modes take from the type:
///
/// ```compile_fail,E0046
/// # use ocx::api::Printable;
/// #[derive(serde::Serialize)]
/// struct Listing {
///     items: Vec<String>,
/// }
/// impl Printable for Listing {
///     const SCHEMA_VERSION: u32 = 1;
///     fn print_plain(&self, _: &ocx_console::DataInterface) {}
/// }
/// ```
///
/// ```compile_fail,E0277
/// # use ocx::command::contract::OutputMode;
/// // A type that is not `Printable` cannot be named as a command's report.
/// struct NotAReport;
/// const MODE: OutputMode = OutputMode::report::<NotAReport>();
/// ```
pub trait Printable: serde::Serialize {
    /// The `schema_version` this root is published under; raised only for a breaking change to its shape.
    const SCHEMA_VERSION: u32;

    /// The name `cli.json` output modes and `reports/v1.json` publish this root under.
    const ROOT: &'static str;

    fn print_plain(&self, data: &DataInterface);
}

/// The one JSON form of a report: `schema_version` first, then the root's own fields.
#[derive(serde::Serialize)]
struct Versioned<'a, T: serde::Serialize> {
    schema_version: u32,
    #[serde(flatten)]
    root: &'a T,
}

/// Receives every published `--format json` root; its name is `T::ROOT`, the one `cli.json` output modes use.
pub trait RootVisitor {
    fn root<T: Printable + schemars::JsonSchema>(&mut self);
}

/// Hands every published report root to `visitor`, in registry order.
pub fn visit_report_roots(visitor: &mut impl RootVisitor) {
    visitor.root::<data::about::About>();
    visitor.root::<data::announce::AnnounceReport>();
    visitor.root::<data::attestation::AttestationReport>();
    visitor.root::<data::catalog::Catalog>();
    visitor.root::<data::claim::ClaimReport>();
    visitor.root::<data::clean::Clean>();
    visitor.root::<data::config_setup::ConfigSetupData>();
    visitor.root::<data::config_test::ConfigTestData>();
    visitor.root::<data::config_update::ConfigUpdateData>();
    visitor.root::<data::deps::Dependencies>();
    visitor.root::<data::deps::DependenciesTrace>();
    visitor.root::<data::deps::FlatDependencies>();
    visitor.root::<data::env::EnvVars>();
    visitor.root::<data::index::CatalogPreview>();
    visitor.root::<data::index::RegenerateReport>();
    visitor.root::<data::install::Installs>();
    visitor.root::<data::lock::LockReport>();
    visitor.root::<data::login::LoginResult>();
    visitor.root::<data::login::LogoutResult>();
    visitor.root::<data::package_cascade_check::PackageCascadeCheck>();
    visitor.root::<data::package_cascade_repair::PackageCascadeRepair>();
    visitor.root::<data::package_copy::CopyReport>();
    visitor.root::<data::package_description::PackageDescriptions>();
    visitor.root::<data::package_inspect::InspectReport>();
    visitor.root::<data::package_prune::PackagePrune>();
    visitor.root::<data::package_receipt::PackageReceipt>();
    visitor.root::<data::patch_freeze::PatchFreezeReport>();
    visitor.root::<data::patch_publish::PatchPublishReport>();
    visitor.root::<data::patch_sync::PatchSyncReport>();
    visitor.root::<data::patch_test::PatchTestReport>();
    visitor.root::<data::patch_why::PatchWhyReport>();
    visitor.root::<data::paths::LocatedPaths>();
    visitor.root::<data::paths::Paths>();
    visitor.root::<data::pull_dry_run::PullDryRun>();
    visitor.root::<data::push::PushReport>();
    visitor.root::<data::removed::Removed>();
    visitor.root::<data::sbom::SbomListingReport>();
    visitor.root::<data::script_run::ScriptRunReport>();
    visitor.root::<data::self_setup::SelfSetupData>();
    visitor.root::<data::self_update::SelfUpdateData>();
    visitor.root::<data::self_update::UpdateCheckData>();
    visitor.root::<data::shell_state::ShellStateReport>();
    visitor.root::<data::shell_state::VerboseShellState>();
    visitor.root::<data::signature::SignatureReport>();
    visitor.root::<data::status::StatusReport>();
    visitor.root::<data::sweep::SweepReport<data::signature::SignatureReport>>();
    visitor.root::<data::sweep::SweepReport<data::attestation::AttestationReport>>();
    visitor.root::<data::tag::Tags>();
    visitor.root::<data::update::UpdateReport>();
    visitor.root::<data::update::VerboseUpdateReport>();
    visitor.root::<data::upgrade::UpgradeReport>();
    visitor.root::<data::upgrade::VerboseUpgradeReport>();
    visitor.root::<data::verification::VerificationReport>();
    visitor.root::<data::version::VerboseVersionData>();
    visitor.root::<data::version::VersionData>();
    visitor.root::<data::warmed_paths::WarmedPaths>();
}

/// Every published report root's `schema_version`, keyed by root name.
pub fn report_versions() -> std::collections::BTreeMap<&'static str, u32> {
    struct Versions(std::collections::BTreeMap<&'static str, u32>);
    impl RootVisitor for Versions {
        fn root<T: Printable + schemars::JsonSchema>(&mut self) {
            self.0.insert(T::ROOT, T::SCHEMA_VERSION);
        }
    }
    let mut versions = Versions(std::collections::BTreeMap::new());
    visit_report_roots(&mut versions);
    versions.0
}

#[derive(Clone)]
pub struct Api {
    format: options::FormatMode,
    data: DataInterface,
    quiet: bool,
    /// Set once a report reaches stdout, shared across clones: the error-envelope wrapper reads it,
    /// or a report-then-fail command would put two JSON documents on stdout.
    reported: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Api {
    pub fn new(format: options::FormatMode, data: DataInterface, quiet: bool) -> Self {
        Self {
            format,
            data,
            quiet,
            reported: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn data(&self) -> &DataInterface {
        &self.data
    }

    /// Whether any report reached stdout; survives the `Context` move into `Command::execute`.
    pub fn reported_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.reported)
    }

    /// Renders `item` to stdout in the configured format; quiet mode suppresses it.
    pub fn report<T: Printable>(&self, item: &T) -> anyhow::Result<()> {
        if self.quiet {
            return Ok(());
        }
        match self.format {
            options::FormatMode::Json => self.data.print_json(&Versioned {
                schema_version: T::SCHEMA_VERSION,
                root: item,
            })?,
            options::FormatMode::Plain => item.print_plain(&self.data),
        }
        self.reported.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn is_json(&self) -> bool {
        matches!(self.format, options::FormatMode::Json)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use ocx_console::Printer;

    use super::*;

    /// Stub `Printable` counting plain renders and JSON serializations, so a test can assert
    /// which form `Api::report` produced.
    struct CallCounter {
        plain: Cell<u32>,
        json: Cell<u32>,
    }

    impl CallCounter {
        fn new() -> Self {
            Self {
                plain: Cell::new(0),
                json: Cell::new(0),
            }
        }
    }

    impl serde::Serialize for CallCounter {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            self.json.set(self.json.get() + 1);
            serializer.serialize_map(Some(0))?.end()
        }
    }

    impl Printable for CallCounter {
        const SCHEMA_VERSION: u32 = 1;
        const ROOT: &'static str = "CallCounter";

        fn print_plain(&self, _data: &DataInterface) {
            self.plain.set(self.plain.get() + 1);
        }
    }

    #[derive(serde::Serialize)]
    struct Root {
        first: u8,
        second: &'static str,
    }

    #[test]
    fn a_versioned_root_leads_with_schema_version_then_its_own_fields() {
        let rendered = serde_json::to_string(&Versioned {
            schema_version: 3,
            root: &Root { first: 1, second: "x" },
        })
        .unwrap();
        assert_eq!(rendered, r#"{"schema_version":3,"first":1,"second":"x"}"#);
    }

    #[test]
    fn report_renders_json_once_when_not_quiet() {
        let api = Api::new(
            options::FormatMode::Json,
            DataInterface::new(Printer::new(false, false)),
            false,
        );
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 0);
        assert_eq!(counter.json.get(), 1);
    }

    #[test]
    fn every_registered_root_has_a_version_of_at_least_one() {
        let versions = report_versions();
        assert_eq!(versions.len(), 56, "{versions:?}");
        assert!(versions.values().all(|version| *version >= 1), "{versions:?}");
    }

    #[test]
    fn report_skips_render_when_quiet() {
        let api = Api::new(
            options::FormatMode::Plain,
            DataInterface::new(Printer::new(false, false)),
            true,
        );
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 0);
        assert_eq!(counter.json.get(), 0);
    }

    #[test]
    fn report_renders_plain_when_not_quiet() {
        let api = Api::new(
            options::FormatMode::Plain,
            DataInterface::new(Printer::new(false, false)),
            false,
        );
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 1);
        assert_eq!(counter.json.get(), 0);
    }

    #[test]
    fn report_skips_json_when_quiet() {
        let api = Api::new(
            options::FormatMode::Json,
            DataInterface::new(Printer::new(false, false)),
            true,
        );
        let counter = CallCounter::new();
        api.report(&counter).unwrap();
        assert_eq!(counter.plain.get(), 0);
        assert_eq!(counter.json.get(), 0);
    }
}
