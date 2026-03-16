// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;
pub(crate) mod flavor;
mod github_flavor;

use crate::package::metadata::env::exporter;

use flavor::Flavor;

/// CI flavors supported by `ocx ci export`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiFlavor {
    /// GitHub Actions: appends to `$GITHUB_PATH` and `$GITHUB_ENV` files.
    GitHubActions,
}

impl CiFlavor {
    /// Detects the current CI environment from well-known environment variables.
    pub fn detect() -> Option<Self> {
        if github_flavor::detect() {
            return Some(Self::GitHubActions);
        }
        None
    }

    /// Writes pre-resolved environment variable entries into the CI system's runtime files.
    pub fn export(self, entries: &[exporter::Entry]) -> crate::Result<()> {
        let mut target: Box<dyn Flavor> = match self {
            Self::GitHubActions => Box::new(github_flavor::GitHubFlavor::from_env()?),
        };

        for entry in entries {
            target.write_entry(&entry.key, &entry.value, &entry.kind)?;
        }

        target.flush()?;
        Ok(())
    }
}

impl std::fmt::Display for CiFlavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitHubActions => write!(f, "GitHub Actions"),
        }
    }
}

impl clap_builder::ValueEnum for CiFlavor {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::GitHubActions]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        Some(match self {
            Self::GitHubActions => clap_builder::builder::PossibleValue::new("github-actions"),
        })
    }
}
