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
///
/// Move-to-front: a value already present in the existing variable is removed
/// from its old position and kept at the front (the buffered `values` precede
/// `existing`, then [`VecExt::unique`](crate::utility::vec_ext::VecExt::unique)
/// keeps the first occurrence). Empty segments are dropped. Re-running an export
/// against an already-exported variable therefore does not grow it.
fn prepend_existing(key: &str, values: &[String]) -> String {
    use crate::utility::vec_ext::VecExt;
    let existing = crate::env::var(key).unwrap_or_default();
    let separator = crate::env::PATH_SEPARATOR;
    let mut parts: Vec<String> = values.to_vec();
    parts.extend(existing.split(separator).map(str::to_string));
    parts.retain(|segment| !segment.is_empty());
    parts.unique();
    parts.join(separator)
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

    // ── prepend_existing move-to-front (idempotency) ─────────────────────
    use crate::env::PATH_SEPARATOR as SEP;

    #[test]
    fn prepend_existing_does_not_re_add_present_value() {
        let env = crate::test::env::lock();
        env.set("PREPEND_TEST", format!("/pkg/bin{SEP}/usr/bin"));
        // `/pkg/bin` is already present → it is moved to front, not duplicated.
        let result = super::prepend_existing("PREPEND_TEST", &["/pkg/bin".to_string()]);
        assert_eq!(result, format!("/pkg/bin{SEP}/usr/bin"));
    }

    #[test]
    fn prepend_existing_new_values_precede_existing() {
        let env = crate::test::env::lock();
        env.set("PREPEND_TEST", "/usr/bin");
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string(), "/b/bin".to_string()]);
        assert_eq!(result, format!("/a/bin{SEP}/b/bin{SEP}/usr/bin"));
    }

    #[test]
    fn prepend_existing_drops_empty_segments() {
        let env = crate::test::env::lock();
        env.set("PREPEND_TEST", format!("/usr/bin{SEP}{SEP}/bin"));
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string()]);
        assert_eq!(result, format!("/a/bin{SEP}/usr/bin{SEP}/bin"));
    }

    #[test]
    fn prepend_existing_collapses_duplicate_new_values() {
        let env = crate::test::env::lock();
        env.remove("PREPEND_TEST");
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string(), "/a/bin".to_string()]);
        assert_eq!(result, "/a/bin");
    }
}
