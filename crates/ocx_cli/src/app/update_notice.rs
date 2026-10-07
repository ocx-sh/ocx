// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one grouped "updates available" block the background checks print before a command runs.

use std::path::Path;

use ocx_console::UserInterface;
use ocx_oci::PackageRef;

use crate::api::data::sanitize_for_terminal;

const TITLE: &str = "ocx: updates available";
/// Names listed per toolchain before the rest collapse into "and N more", so the block never grows with drift.
const MAX_NAMES: usize = 3;

/// One line of the block: what moved, the command that takes it, and the command that shows why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoticeRow {
    text: String,
    command: &'static str,
    details: Option<&'static str>,
}

impl NoticeRow {
    /// A newer ocx release.
    pub(crate) fn ocx(identifier: &PackageRef) -> Self {
        let version = identifier.tag().map_or_else(|| identifier.to_string(), str::to_owned);
        Self {
            text: format!("ocx {}", sanitize_for_terminal(&version)),
            command: "ocx self update",
            details: None,
        }
    }

    /// Drifted tools in the project toolchain rooted at `dir`; `None` when nothing moved.
    pub(crate) fn project(dir: &Path, names: &[&str]) -> Option<Self> {
        let home = ocx_env::home_dir();
        let dir = sanitize_for_terminal(&shorten_home(dir, home.as_deref()));
        Some(Self {
            text: format!("project ({dir}): {}", tool_list(names)?),
            command: "ocx update",
            details: Some("ocx update --check"),
        })
    }

    /// Drifted tools in the global toolchain; `None` when nothing moved.
    pub(crate) fn global(names: &[&str]) -> Option<Self> {
        Some(Self {
            text: format!("global: {}", tool_list(names)?),
            command: "ocx --global update",
            details: Some("ocx --global update --check"),
        })
    }
}

/// Prints every row as one block on stderr, closed by the toolchains' detail commands; nothing for no rows.
pub(crate) fn print(ui: &UserInterface, rows: &[NoticeRow]) {
    let lines: Vec<_> = rows.iter().map(|row| (row.text.as_str(), row.command)).collect();
    let details: Vec<_> = rows.iter().filter_map(|row| row.details).collect();
    ui.notice(TITLE, &lines, &details);
}

/// Sorted, sanitized names, at most [`MAX_NAMES`] then "and N more"; `None` for no names.
fn tool_list(names: &[&str]) -> Option<String> {
    let mut names: Vec<String> = names.iter().map(|name| sanitize_for_terminal(name)).collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return None;
    }
    let shown = names
        .iter()
        .take(MAX_NAMES)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Some(match names.len().saturating_sub(MAX_NAMES) {
        0 => shown,
        rest => format!("{shown} and {rest} more"),
    })
}

/// `dir` with a leading `home` replaced by `~`.
fn shorten_home(dir: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| dir.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => Path::new("~").join(rest).display().to_string(),
        None => dir.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: [&str; 10] = [
        "uv", "ninja", "cmake", "zig", "go", "bun", "jq", "lychee", "shfmt", "task",
    ];

    #[test]
    fn no_tools_is_no_row() {
        assert_eq!(tool_list(&[]), None);
        assert_eq!(NoticeRow::global(&[]), None);
        assert_eq!(NoticeRow::project(Path::new("/work"), &[]), None);
    }

    #[test]
    fn one_tool_is_named_alone() {
        assert_eq!(tool_list(&["shellcheck"]).as_deref(), Some("shellcheck"));
    }

    #[test]
    fn three_tools_are_all_named_in_sorted_order() {
        assert_eq!(
            tool_list(&["uv", "ninja", "cmake"]).as_deref(),
            Some("cmake, ninja, uv")
        );
    }

    #[test]
    fn ten_tools_name_the_first_three_sorted_and_count_the_rest() {
        assert_eq!(tool_list(&TOOLS).as_deref(), Some("bun, cmake, go and 7 more"));
    }

    #[test]
    fn home_is_shortened_to_a_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(
            shorten_home(&home.join("work").join("proj"), Some(home)),
            Path::new("~").join("work").join("proj").display().to_string()
        );
        assert_eq!(shorten_home(home, Some(home)), "~");
        assert_eq!(shorten_home(Path::new("/srv/proj"), Some(home)), "/srv/proj");
        assert_eq!(shorten_home(Path::new("/srv/proj"), None), "/srv/proj");
    }

    #[test]
    fn rows_name_their_scope_and_command() {
        let project = NoticeRow::project(Path::new("/srv/proj"), &["uv", "ninja", "cmake", "zig"]);
        assert_eq!(
            project,
            Some(NoticeRow {
                text: "project (/srv/proj): cmake, ninja, uv and 1 more".to_owned(),
                command: "ocx update",
                details: Some("ocx update --check"),
            })
        );
        let global = NoticeRow::global(&["shellcheck"]);
        assert_eq!(
            global,
            Some(NoticeRow {
                text: "global: shellcheck".to_owned(),
                command: "ocx --global update",
                details: Some("ocx --global update --check"),
            })
        );
        let identifier = ocx_oci::ocx_cli_identifier().clone_with_tag("0.7.0".to_string());
        assert_eq!(
            NoticeRow::ocx(&identifier),
            NoticeRow {
                text: "ocx 0.7.0".to_owned(),
                command: "ocx self update",
                details: None,
            }
        );
    }

    /// Names and the project path come from a cloned repository's files: no ESC or bidi override reaches the terminal.
    #[test]
    fn rows_strip_escape_and_bidi_controls() {
        let row = NoticeRow::project(
            Path::new("/work/\u{1b}]0;x\u{7}"),
            &["cm\u{1b}[31make", "nin\u{202e}ja"],
        )
        .expect("drift is a row");
        for control in ['\u{1b}', '\u{202e}', '\u{7}'] {
            assert!(!row.text.contains(control), "{row:?}");
        }
    }
}
