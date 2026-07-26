// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// The repeatable `-g` / `--group` scope selector.
///
/// Flatten into a command with `#[clap(flatten)]` to add the flag, then read
/// the selection through [`GroupSelection::names`].
///
/// **Project-toolchain tier only.** Groups are declared in `ocx.toml`, so an
/// OCI-tier command has nothing to select and must not flatten this struct —
/// the flag would parse and silently do nothing.
///
/// Validation is deliberately not a method here, because the two checks belong
/// to different phases of a command: `project_context::ensure_group_segments_nonempty`
/// rejects empty comma segments before any filesystem or network work, and
/// `project_context::ensure_groups_known` rejects unknown names once the config
/// is loaded.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct GroupSelection {
    /// Restrict the composition to the named group(s).
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
    /// The requested group names, in `-g` order, exactly as typed.
    ///
    /// Reserved names (`default`, `all`) are not expanded here — expansion is
    /// `expand_all_keyword`'s job and needs the loaded config.
    pub fn names(&self) -> &[String] {
        &self.groups
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
