// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_project::{DEFAULT_GROUP, ProjectConfig, expand_all_keyword};

/// The repeatable `-g` / `--group` scope selector, read through
/// [`GroupSelection::names`].
// Project-tier commands only: groups live in `ocx.toml`, so on an OCI-tier command the flag silently does nothing.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct GroupSelection {
    /// Restrict to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The reserved
    /// name `default` selects the top-level `[tools]` table. The reserved name
    /// `all` expands to `default` plus every declared `[group.*]`. When
    /// omitted, scope is exactly `[tools]` - an omitted `-g` does not mean
    /// "everything", it means "the default group".
    ///
    /// A group's own `[env]` composes after the project's, in the order the
    /// groups are given, so a later `-g` wins.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    groups: Vec<String>,
}

impl GroupSelection {
    /// The requested group names in `-g` order, unexpanded (`expand_all_keyword` expands `default`/`all`).
    pub fn names(&self) -> &[String] {
        &self.groups
    }

    /// The groups to act on: `-g` validated against `config`, `all` expanded, and an omitted `-g` read as `default`.
    ///
    /// # Errors
    ///
    /// Exit 64 for an unknown group name.
    pub fn effective(&self, config: &ProjectConfig) -> anyhow::Result<Vec<String>> {
        crate::app::project_context::ensure_groups_known(&self.groups, config)?;
        Ok(expand_with_default(&self.groups, config))
    }
}

/// [`expand_all_keyword`] with an empty selection read as the default group; unvalidated.
pub(crate) fn expand_with_default(names: &[String], config: &ProjectConfig) -> Vec<String> {
    let expanded = expand_all_keyword(names, config);
    if expanded.is_empty() {
        vec![DEFAULT_GROUP.to_owned()]
    } else {
        expanded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        groups: GroupSelection,
    }

    fn names(arguments: &[&str]) -> Vec<String> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(arguments);
        Harness::try_parse_from(argv).expect("parse").groups.names().to_vec()
    }

    /// No flag → an empty selection; the caller supplies the default scope.
    #[test]
    fn absent_flag_selects_nothing() {
        assert!(names(&[]).is_empty(), "an omitted -g must not invent a group");
    }

    /// Both the repeated and the comma-separated spellings accumulate, and the
    /// `-g` order is preserved because it decides which group's `[env]` wins.
    #[test]
    fn repeated_and_comma_forms_accumulate_in_order() {
        assert_eq!(names(&["-g", "ci,lint", "-g", "release"]), ["ci", "lint", "release"]);
        assert_eq!(names(&["--group", "b", "--group", "a"]), ["b", "a"]);
    }

    /// A stray comma survives parsing as an empty segment — it is a user-typing
    /// error the command rejects in its parse-time phase, not something clap
    /// filters out. Pinned so that check cannot be dropped as redundant.
    #[test]
    fn empty_segment_reaches_the_caller() {
        assert_eq!(names(&["-g", "ci,,lint"]), ["ci", "", "lint"]);
    }
}
