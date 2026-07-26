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
/// Reads the existing process value of `key` and folds each buffered package
/// value from `values` (in accumulation order) through
/// [`move_to_front`](crate::utility::path::move_to_front) — the same
/// operation `Env::add_path` applies once per entry on the in-process
/// `ocx run` path. This is the one place the env-read + move-to-front
/// semantics live for CI export, so GitHub's `$GITHUB_ENV` path case and
/// GitLab's flattened path case stay byte-identical *and* agree with
/// `ocx run`'s in-process precedence.
///
/// **Last-applied wins, landing at the front.** `values` accumulate in the
/// same order `write_entry` is called (earliest-processed package or stage
/// first). Folding `move_to_front` in that order means the value applied
/// *last* ends up closest to the front — a value already present (in an
/// earlier `values` entry, or in `existing`) is removed from its old position
/// and kept at the new front. Empty segments are dropped. Re-running an
/// export against an already-exported variable therefore does not grow it.
///
/// **Direction fixed by `adr_project_env_declaration.md` C1a.** This used to
/// prepend the whole buffered block and keep the *first* occurrence, so the
/// *first*-applied value held the front — the inverse of `Env::add_path`'s
/// `ocx run` semantics. Under `--ci=github`/`--ci=gitlab` a later stage could
/// not override an earlier one; it now can, matching `ocx run`.
fn prepend_existing(key: &str, values: &[String]) -> String {
    use std::ffi::{OsStr, OsString};

    use crate::utility::path::move_to_front;

    let existing = crate::env::var(key).unwrap_or_default();
    let mut result = OsString::from(existing);
    for value in values {
        result = move_to_front(&result, OsStr::new(value));
    }
    result.to_string_lossy().into_owned()
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

    // ── prepend_existing move-to-front (last-applied-wins) ────────────────
    //
    // C1a fix (`adr_project_env_declaration.md`): among several path values
    // for one key, the value applied *last* now lands at the front, matching
    // `Env::add_path`'s `ocx run` semantics — the inverse of the old
    // first-applied-wins direction. These four tests pin the fixed
    // direction; the first three assertions are unchanged by the flip
    // (single-value / identical-duplicate cases are order-independent), the
    // second one changes value and is the direct regression pin.
    use crate::env::PATH_SEPARATOR as SEP;

    #[test]
    fn prepend_existing_does_not_re_add_present_value() {
        // A single value already present is moved to front, not duplicated.
        // Order-independent under both the old and the fixed direction.
        let env = crate::test::env::lock();
        env.set("PREPEND_TEST", format!("/pkg/bin{SEP}/usr/bin"));
        let result = super::prepend_existing("PREPEND_TEST", &["/pkg/bin".to_string()]);
        assert_eq!(result, format!("/pkg/bin{SEP}/usr/bin"));
    }

    #[test]
    fn prepend_existing_last_applied_value_precedes_earlier_values() {
        // Two distinct path contributions for the same key, applied in
        // sequence: `/a/bin` first, `/b/bin` second. Pins last-applied-wins:
        // `/b/bin` lands at the front. Fails under the old first-wins
        // direction, which put `/a/bin` (applied first) at the front.
        let env = crate::test::env::lock();
        env.set("PREPEND_TEST", "/usr/bin");
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string(), "/b/bin".to_string()]);
        assert_eq!(result, format!("/b/bin{SEP}/a/bin{SEP}/usr/bin"));
    }

    #[test]
    fn prepend_existing_drops_empty_segments() {
        // Empty segments in the existing value are dropped regardless of
        // direction — order-independent under both the old and the fixed
        // direction (only one new value here).
        let env = crate::test::env::lock();
        env.set("PREPEND_TEST", format!("/usr/bin{SEP}{SEP}/bin"));
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string()]);
        assert_eq!(result, format!("/a/bin{SEP}/usr/bin{SEP}/bin"));
    }

    #[test]
    fn prepend_existing_collapses_duplicate_new_values() {
        // Identical repeated values collapse to a single front occurrence
        // regardless of direction — order-independent under both the old and
        // the fixed direction (no distinct values to reorder).
        let env = crate::test::env::lock();
        env.remove("PREPEND_TEST");
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string(), "/a/bin".to_string()]);
        assert_eq!(result, "/a/bin");
    }

    #[test]
    fn prepend_existing_last_applied_wins_over_existing_process_value() {
        // Regression pin for C1a: a key already set in the existing process
        // env, then two distinct path contributions for the same key applied
        // in sequence. The value applied last (`/b/bin`) must be at the
        // front, ahead of both the earlier contribution and the pre-existing
        // value. Fails under the old first-wins behavior, which would have
        // put `/a/bin` first instead.
        let env = crate::test::env::lock();
        env.set("PREPEND_TEST", "/existing/bin");
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string(), "/b/bin".to_string()]);
        assert_eq!(result, format!("/b/bin{SEP}/a/bin{SEP}/existing/bin"));
    }
}
