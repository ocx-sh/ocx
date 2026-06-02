// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;
pub(crate) mod flavor;
mod github_flavor;
mod gitlab_flavor;

use std::path::PathBuf;

use crate::package::metadata::env::entry;

use flavor::Flavor;

/// CI flavors supported by the `--ci` flag on `ocx env` / `ocx package env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiFlavor {
    /// GitHub Actions: appends to the `$GITHUB_PATH` and `$GITHUB_ENV` files.
    GitHubActions,
    /// GitLab CI/CD: writes JSON-lines to an export file (or stdout) consumed
    /// by later pipeline steps.
    GitLab,
}

impl CiFlavor {
    /// Detects the current CI environment from well-known environment variables.
    pub fn detect() -> Option<Self> {
        if github_flavor::detect() {
            return Some(Self::GitHubActions);
        }
        if gitlab_flavor::detect() {
            return Some(Self::GitLab);
        }
        None
    }

    /// Writes pre-resolved environment variable entries into the CI system's
    /// runtime channel.
    ///
    /// `export_file` selects an explicit GitLab output path; it is ignored for
    /// GitHub Actions (which infers its two-file sink from `$GITHUB_ENV` /
    /// `$GITHUB_PATH`) and the caller is expected to reject the combination.
    pub fn export(self, entries: &[entry::Entry], export_file: Option<PathBuf>) -> crate::Result<()> {
        let mut target: Box<dyn Flavor> = match self {
            Self::GitHubActions => Box::new(github_flavor::GitHubFlavor::from_env()?),
            Self::GitLab => Box::new(gitlab_flavor::GitLabFlavor::new(export_file)?),
        };

        for entry in entries {
            target.write_entry(&entry.key, &entry.value, &entry.kind)?;
        }

        target.flush()?;
        Ok(())
    }
}

/// Computes the final value for a path-type variable shared by both CI flavors.
///
/// Reads the existing process value of `key`, prepends the buffered package
/// `values` (in accumulation order), and joins everything with
/// [`PATH_SEPARATOR`](crate::env::PATH_SEPARATOR). This is the one place the
/// env-read + separator-join semantics live, so GitHub's `$GITHUB_ENV` path
/// case and GitLab's flattened path case stay byte-identical.
fn prepend_existing(key: &str, values: &[String]) -> String {
    let existing = crate::env::var(key).unwrap_or_default();
    let mut parts: Vec<&str> = values.iter().map(String::as_str).collect();
    if !existing.is_empty() {
        parts.push(&existing);
    }
    parts.join(crate::env::PATH_SEPARATOR)
}

impl std::fmt::Display for CiFlavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitHubActions => write!(f, "GitHub Actions"),
            Self::GitLab => write!(f, "GitLab CI"),
        }
    }
}

impl clap_builder::ValueEnum for CiFlavor {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::GitHubActions, Self::GitLab]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        Some(match self {
            Self::GitHubActions => clap_builder::builder::PossibleValue::new("github").alias("github-actions"),
            Self::GitLab => clap_builder::builder::PossibleValue::new("gitlab").alias("gitlab-ci"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CiFlavor;
    use clap_builder::ValueEnum;

    #[test]
    fn value_enum_parses_github_and_alias() {
        assert_eq!(CiFlavor::from_str("github", false).unwrap(), CiFlavor::GitHubActions);
        assert_eq!(
            CiFlavor::from_str("github-actions", false).unwrap(),
            CiFlavor::GitHubActions
        );
    }

    #[test]
    fn value_enum_parses_gitlab_and_alias() {
        assert_eq!(CiFlavor::from_str("gitlab", false).unwrap(), CiFlavor::GitLab);
        assert_eq!(CiFlavor::from_str("gitlab-ci", false).unwrap(), CiFlavor::GitLab);
    }

    #[test]
    fn value_enum_rejects_unknown() {
        assert!(CiFlavor::from_str("jenkins", false).is_err());
    }
}
