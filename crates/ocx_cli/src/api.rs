// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::options;

pub mod data;

/// Implemented by API data types that know how to render themselves in either output format.
///
/// The `report` method on [`Api`] dispatches between JSON and plain text via
/// this trait, so each data type owns its own formatting logic rather than
/// delegating it to a giant match block in the API layer.
///
/// `print_json` has a default implementation (pretty-printed `serde_json`);
/// override it only when a non-standard JSON representation is required.
pub trait Reportable: serde::Serialize {
    fn print_plain(&self);

    fn print_json(&self) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string_pretty(self)?);
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct Api {
    format: options::Format,
}

impl Api {
    pub fn new(format: options::Format) -> Self {
        Self { format }
    }

    fn report<T: Reportable>(&self, item: &T) -> anyhow::Result<()> {
        match self.format {
            options::Format::Json => item.print_json()?,
            options::Format::Plain => item.print_plain(),
        }
        Ok(())
    }

    pub fn report_ci_exported(&self, exported: data::ci_export::CiExported) -> anyhow::Result<()> {
        self.report(&exported)
    }

    pub fn report_installs(&self, installs: data::install::Installs) -> anyhow::Result<()> {
        self.report(&installs)
    }

    pub fn report_tags(&self, tags: data::tag::Tags) -> anyhow::Result<()> {
        self.report(&tags)
    }

    pub fn report_env(&self, env: data::env::EnvVars) -> anyhow::Result<()> {
        self.report(&env)
    }

    pub fn report_catalog(&self, catalog: data::catalog::Catalog) -> anyhow::Result<()> {
        self.report(&catalog)
    }

    pub fn report_paths(&self, paths: data::paths::Paths) -> anyhow::Result<()> {
        self.report(&paths)
    }

    pub fn report_removed(&self, removed: data::removed::Removed) -> anyhow::Result<()> {
        self.report(&removed)
    }

    pub fn report_clean(&self, clean: data::clean::Clean) -> anyhow::Result<()> {
        self.report(&clean)
    }

    pub fn report_info(&self, info: data::info::Info) -> anyhow::Result<()> {
        self.report(&info)
    }

    pub fn report_package_description(
        &self,
        desc: data::package_description::PackageDescription,
    ) -> anyhow::Result<()> {
        self.report(&desc)
    }

    pub fn report_profile(&self, profile: data::profile::ProfileList) -> anyhow::Result<()> {
        self.report(&profile)
    }

    pub fn report_profile_added(&self, added: data::profile_added::ProfileAdded) -> anyhow::Result<()> {
        self.report(&added)
    }

    pub fn report_profile_removed(&self, removed: data::profile_removed::ProfileRemoved) -> anyhow::Result<()> {
        self.report(&removed)
    }

    pub fn is_json(&self) -> bool {
        matches!(self.format, options::Format::Json)
    }
}
