// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod annotations;
pub mod error;
pub(crate) mod flavor;
mod github_flavor;
mod gitlab_flavor;

use std::path::PathBuf;

use ocx_package::metadata::env::entry;

use flavor::Flavor;

/// CI flavors supported by the `--ci` flag on `ocx env` / `ocx package env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiFlavor {
    /// GitHub Actions: appends to the `$GITHUB_PATH` and `$GITHUB_ENV` files.
    GitHubActions,
    /// GitLab CI/CD: writes JSON lines to an export file or stdout.
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

    /// Writes resolved entries into the CI system's runtime channel.
    ///
    /// `export_file` is the GitLab output path; GitHub ignores it, so the caller must reject that combination.
    pub fn export(self, entries: &[entry::Entry], export_file: Option<PathBuf>) -> Result<(), crate::ci::error::Error> {
        let mut target: Box<dyn Flavor> = match self {
            Self::GitHubActions => Box::new(github_flavor::GitHubFlavor::from_env()?),
            Self::GitLab => Box::new(gitlab_flavor::GitLabFlavor::new(export_file)?),
        };

        for entry in entries {
            target.write_entry(&entry.key, &entry.value, &entry.kind, entry.separator.as_deref())?;
        }

        target.flush()?;
        Ok(())
    }
}

/// Folds `values` onto the process value of path variable `key` with the same
/// [`move_to_front`](ocx_util::path::move_to_front) `ocx exec` uses, so CI precedence matches it.
/// A declared secret reads as absent, so its value never reaches the export file.
fn prepend_existing(key: &str, values: &[String]) -> String {
    use std::ffi::{OsStr, OsString};

    use ocx_util::path::move_to_front;

    let existing = ocx_env::dynamic(key).unwrap_or_default();
    let mut result = OsString::from(existing);
    for value in values {
        result = move_to_front(&result, OsStr::new(value));
    }
    result.to_string_lossy().into_owned()
}

/// Folds `values` onto the process value of list variable `key` with the same
/// [`append_unique`](ocx_util::list::append_unique) `ocx exec` uses, so CI precedence matches it.
/// A declared secret reads as absent, so its value never reaches the export file.
fn append_existing(key: &str, values: &[String], separator: &str) -> String {
    use ocx_util::list::append_unique;

    let mut result = ocx_env::dynamic(key).unwrap_or_default();
    for value in values {
        result = append_unique(&result, value, separator);
    }
    result
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
    // `Env::add_path`'s `ocx exec` semantics — the inverse of the old
    // first-applied-wins direction. These four tests pin the fixed
    // direction; the first three assertions are unchanged by the flip
    // (single-value / identical-duplicate cases are order-independent), the
    // second one changes value and is the direct regression pin.
    use ocx_util::path::PATH_SEPARATOR as SEP;

    #[test]
    fn prepend_existing_does_not_re_add_present_value() {
        // A single value already present is moved to front, not duplicated.
        // Order-independent under both the old and the fixed direction.
        let env = ocx_env::overrides::lock();
        env.set_raw("PREPEND_TEST", format!("/pkg/bin{SEP}/usr/bin"));
        let result = super::prepend_existing("PREPEND_TEST", &["/pkg/bin".to_string()]);
        assert_eq!(result, format!("/pkg/bin{SEP}/usr/bin"));
    }

    #[test]
    fn prepend_existing_last_applied_value_precedes_earlier_values() {
        // Two distinct path contributions for the same key, applied in
        // sequence: `/a/bin` first, `/b/bin` second. Pins last-applied-wins:
        // `/b/bin` lands at the front. Fails under the old first-wins
        // direction, which put `/a/bin` (applied first) at the front.
        let env = ocx_env::overrides::lock();
        env.set_raw("PREPEND_TEST", "/usr/bin");
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string(), "/b/bin".to_string()]);
        assert_eq!(result, format!("/b/bin{SEP}/a/bin{SEP}/usr/bin"));
    }

    #[test]
    fn prepend_existing_drops_empty_segments() {
        // Empty segments in the existing value are dropped regardless of
        // direction — order-independent under both the old and the fixed
        // direction (only one new value here).
        let env = ocx_env::overrides::lock();
        env.set_raw("PREPEND_TEST", format!("/usr/bin{SEP}{SEP}/bin"));
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string()]);
        assert_eq!(result, format!("/a/bin{SEP}/usr/bin{SEP}/bin"));
    }

    #[test]
    fn prepend_existing_collapses_duplicate_new_values() {
        // Identical repeated values collapse to a single front occurrence
        // regardless of direction — order-independent under both the old and
        // the fixed direction (no distinct values to reorder).
        let env = ocx_env::overrides::lock();
        env.remove_raw("PREPEND_TEST");
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
        let env = ocx_env::overrides::lock();
        env.set_raw("PREPEND_TEST", "/existing/bin");
        let result = super::prepend_existing("PREPEND_TEST", &["/a/bin".to_string(), "/b/bin".to_string()]);
        assert_eq!(result, format!("/b/bin{SEP}/a/bin{SEP}/existing/bin"));
    }

    // ── append_existing move-to-back (last-applied-wins) ───────────────────
    //
    // W-9: the append-direction sibling of `prepend_existing`. A `list`
    // contribution moves to the BACK on re-application — the opposite end
    // from `path`'s front — matching `Env::add_list`'s `ocx exec` semantics.

    #[test]
    fn append_existing_on_an_empty_ambient_yields_the_bare_value() {
        let env = ocx_env::overrides::lock();
        env.remove_raw("APPEND_TEST");
        let result = super::append_existing("APPEND_TEST", &["-ea".to_string()], " ");
        assert_eq!(result, "-ea");
    }

    #[test]
    fn append_existing_moves_a_duplicate_contribution_to_the_back() {
        // A value already present (from the ambient process env) is removed
        // from its old position and re-appended at the back, so re-exporting
        // an already-exported variable does not grow it.
        let env = ocx_env::overrides::lock();
        env.set_raw("APPEND_TEST", "-ea -Xmx1g");
        let result = super::append_existing("APPEND_TEST", &["-ea".to_string()], " ");
        assert_eq!(result, "-Xmx1g -ea");
    }

    #[test]
    fn append_existing_last_applied_value_follows_earlier_values() {
        // Two distinct list contributions for the same key, applied in
        // sequence: `-ea` first, `-server` second. Last-applied-wins lands
        // `-server` at the back, after `-ea`.
        let env = ocx_env::overrides::lock();
        env.remove_raw("APPEND_TEST");
        let result = super::append_existing("APPEND_TEST", &["-ea".to_string(), "-server".to_string()], " ");
        assert_eq!(result, "-ea -server");
    }

    #[test]
    fn append_existing_uses_the_entrys_own_separator() {
        // `GODEBUG`-style comma separator, distinct from the space default.
        let env = ocx_env::overrides::lock();
        env.remove_raw("APPEND_TEST");
        let result = super::append_existing(
            "APPEND_TEST",
            &["gctrace=1".to_string(), "madvdontneed=1".to_string()],
            ",",
        );
        assert_eq!(result, "gctrace=1,madvdontneed=1");
    }

    #[test]
    fn an_existing_ci_secret_is_never_folded_into_an_exported_value() {
        let env = ocx_env::overrides::lock();
        env.set(ocx_env::CI_JOB_TOKEN.declaration(), "glcbt-hunter2");
        let prepended = super::prepend_existing("CI_JOB_TOKEN", &["/pkg/bin".to_string()]);
        let appended = super::append_existing("CI_JOB_TOKEN", &["-ea".to_string()], " ");
        assert_eq!(prepended, "/pkg/bin");
        assert_eq!(appended, "-ea");
    }
}
