// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Deprecated command and flag spellings for the 0.6-to-0.7 pair, deleted whole in 0.7 with the
//! hidden variants that call it, so nothing else may depend on it.
//!
//! Each command spelling is a hidden command, never a clap alias: `ArgMatches` reports only the
//! canonical name, so an alias could never warn. Each flag spelling is a hidden argument whose id
//! [`RenamedFlag::arg_id`] derives, read back by [`renamed_flags_used`]. The removal release is the
//! `REMOVAL_RELEASE` constant every row's notice reads.

use clap::ArgMatches;
use clap::parser::ValueSource;
use ocx_env::REMOVAL_RELEASE;

use super::leaf::Leaf;
use crate::app::Context;

/// A flag spelled as typed on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spelling {
    Long(&'static str),
    Short(char),
}

impl Spelling {
    /// Whether an argument named `long` and `short` is spelled this way.
    #[must_use]
    pub fn is_spelled(&self, long: Option<&str>, short: Option<char>) -> bool {
        match *self {
            Self::Long(name) => long == Some(name),
            Self::Short(letter) => short == Some(letter),
        }
    }
}

impl std::fmt::Display for Spelling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Long(name) => write!(f, "--{name}"),
            Self::Short(letter) => write!(f, "-{letter}"),
        }
    }
}

/// A hidden command spelling that still runs the command it was renamed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenamedCommand {
    pub old: Leaf,
    pub new: Leaf,
}

/// A hidden flag spelling that still works on the command `path` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenamedFlag {
    pub path: Leaf,
    pub old: Spelling,
    pub new: Spelling,
    id: &'static str,
}

impl RenamedFlag {
    /// The clap id of the hidden argument that keeps the old spelling working: `--out` is `deprecated_out`,
    /// `-c` is `deprecated_c`.
    #[must_use]
    pub const fn arg_id(&self) -> &'static str {
        self.id
    }
}

/// One flag row; the hidden argument's id is built from the old spelling here and nowhere else.
macro_rules! renamed_flag {
    ($path:ident, $old_kind:ident($old:literal) => $new_kind:ident($new:literal)) => {
        RenamedFlag {
            path: Leaf::$path,
            old: Spelling::$old_kind($old),
            new: Spelling::$new_kind($new),
            id: concat!("deprecated_", $old),
        }
    };
}

pub const RUN: RenamedCommand = RenamedCommand {
    old: Leaf::Run,
    new: Leaf::Exec,
};
pub const PACKAGE_DESCRIBE: RenamedCommand = RenamedCommand {
    old: Leaf::PackageDescribe,
    new: Leaf::PackageDescriptionPush,
};
pub const PACKAGE_INFO: RenamedCommand = RenamedCommand {
    old: Leaf::PackageInfo,
    new: Leaf::PackageDescriptionPull,
};

pub const CONFIG_PUSH_C: RenamedFlag = renamed_flag!(ConfigPush, Short('c') => Long("cascade"));
pub const INDEX_CATALOG_TAGS: RenamedFlag = renamed_flag!(IndexCatalog, Long("tags") => Long("with-tags"));
/// `package claim --out` shares the options struct, and so the hidden argument, with this row.
pub const ANNOUNCE_OUT: RenamedFlag = renamed_flag!(PackageAnnounce, Long("out") => Long("output"));
pub const CLAIM_OUT: RenamedFlag = RenamedFlag {
    path: Leaf::PackageClaim,
    ..ANNOUNCE_OUT
};
pub const COPY_C: RenamedFlag = renamed_flag!(PackageCopy, Short('c') => Long("cascade"));
pub const COPY_DESCRIPTION: RenamedFlag = renamed_flag!(PackageCopy, Long("description") => Long("with-description"));
pub const CREATE_L: RenamedFlag = renamed_flag!(PackageCreate, Short('l') => Long("compression-level"));
pub const PUSH_C: RenamedFlag = renamed_flag!(PackagePush, Short('c') => Long("cascade"));

/// Every command spelling this window renames.
pub const COMMANDS: &[RenamedCommand] = &[RUN, PACKAGE_DESCRIBE, PACKAGE_INFO];

/// Every flag spelling this window renames. The renamed announce `--package` flag became a positional,
/// not a spelling, so it is not a row.
pub const FLAGS: &[RenamedFlag] = &[
    CONFIG_PUSH_C,
    INDEX_CATALOG_TAGS,
    ANNOUNCE_OUT,
    CLAIM_OUT,
    COPY_C,
    COPY_DESCRIPTION,
    CREATE_L,
    PUSH_C,
];

fn words(leaf: Leaf) -> String {
    leaf.path().join(" ")
}

impl RenamedCommand {
    /// The warning for this spelling.
    #[must_use]
    pub fn notice(&self) -> String {
        notice(&words(self.old), &words(self.new))
    }
}

impl RenamedFlag {
    /// The command path and old spelling, as typed (`package push -c`).
    #[must_use]
    pub fn old_text(&self) -> String {
        format!("{} {}", words(self.path), self.old)
    }

    /// The command path and replacement spelling (`package push --cascade`).
    #[must_use]
    pub fn new_text(&self) -> String {
        format!("{} {}", words(self.path), self.new)
    }

    /// The warning for this spelling.
    #[must_use]
    pub fn notice(&self) -> String {
        notice(&self.old_text(), &self.new_text())
    }
}

/// The flag rows of [`FLAGS`] the command line spelled the old way, in table order.
#[must_use]
pub fn renamed_flags_used(matches: &ArgMatches) -> Vec<RenamedFlag> {
    let mut path = Vec::new();
    let mut leaf = matches;
    while let Some((name, sub)) = leaf.subcommand() {
        path.push(name);
        leaf = sub;
    }
    FLAGS
        .iter()
        .copied()
        .filter(|row| {
            let id = row.arg_id();
            // `try_contains_id` first: `value_source` panics on an id the command lacks.
            row.path.path() == path.as_slice()
                && leaf.try_contains_id(id).is_ok()
                && leaf.value_source(id) == Some(ValueSource::CommandLine)
        })
        .collect()
}

fn notice(old: &str, new: &str) -> String {
    format!(
        "`ocx {old}` is renamed to `ocx {new}` and is removed in {}",
        REMOVAL_RELEASE.minor_form()
    )
}

/// Warn on stderr that a command spelling has been renamed; once, as one process dispatches one command.
pub fn warn_renamed(context: &Context, row: &RenamedCommand) {
    context.ui().warn(row.notice());
}

/// Warn on stderr once for each renamed flag the command line spelled the old way.
pub fn warn_renamed_flags(context: &Context, matches: &ArgMatches) {
    for row in renamed_flags_used(matches) {
        context.ui().warn(row.notice());
    }
}

/// The notice `ocx package announce --package` prints. Returned rather than warned so a test can
/// assert it; the caller routes it through [`Context::ui`] and owns once-ness.
#[must_use]
pub fn package_flag_notice() -> String {
    format!(
        "`ocx package announce --package <PACKAGE>` takes the package as a positional argument now; \
         the flag is removed in {}",
        REMOVAL_RELEASE.minor_form()
    )
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::{
        ANNOUNCE_OUT, CLAIM_OUT, CONFIG_PUSH_C, COPY_C, COPY_DESCRIPTION, CREATE_L, FLAGS, INDEX_CATALOG_TAGS, PUSH_C,
        RenamedFlag, package_flag_notice, renamed_flags_used,
    };

    /// The notice names the deprecated spelling, the form that replaces it, and the
    /// release that removes it.
    ///
    /// This is the **reachable half** of the inventory's
    /// `announce_warning_never_reaches_stdout`. No Rust seam in this
    /// workspace observes stdout — [`ocx_console::Printer`] writes the real
    /// streams and [`ocx_console::Cell`] exposes no text — so the stdout-purity
    /// and once-ness halves are acceptance assertions instead, in
    /// `test/tests/test_announce.py::test_deprecated_package_flag_warns_once_on_stderr_only`,
    /// whose mechanism is already proved in both directions at
    /// `test/tests/test_tag_reserved.py:131,137` (a `ui().warn` on stderr while
    /// stdout stays parseable JSON in the same run).
    ///
    /// `"0.7"` is quoted as a **literal**, never read off `ocx_env::REMOVAL_RELEASE`:
    /// reading the constant would make the named mutation invisible, because the
    /// expectation would move with it.
    ///
    /// Mutation: set `REMOVAL_RELEASE` to 0.8; or drop the word `positional` from
    /// the sentence, which is the needle the acceptance half counts.
    #[test]
    fn announce_package_flag_notice_names_the_removal_release() {
        let notice = package_flag_notice();
        assert!(
            notice.contains("--package"),
            "the notice must name the spelling the operator typed, got: {notice}"
        );
        assert!(
            notice.contains("positional"),
            "the notice must name the form that replaces it, got: {notice}"
        );
        assert!(
            notice.contains("0.7"),
            "the notice must name the release that removes the flag, got: {notice}"
        );
    }

    /// One command line per flag row of [`FLAGS`], spelled the old way; the
    /// new spelling is the row's `new` substituted in.
    const FLAG_ROW_ARGV: &[(RenamedFlag, &[&str])] = &[
        (
            CONFIG_PUSH_C,
            &["config", "push", "-c", "-i", "r.example/c:1", "config.toml"],
        ),
        (INDEX_CATALOG_TAGS, &["index", "catalog", "--tags"]),
        (
            ANNOUNCE_OUT,
            &["package", "announce", "--tags", "1", "--out", "d", "acme/widget"],
        ),
        (
            CLAIM_OUT,
            &[
                "package",
                "claim",
                "--repository",
                "https://example.com/r",
                "--out",
                "d",
                "acme/widget",
            ],
        ),
        (COPY_C, &["package", "copy", "-c", "--to", "r.example", "r.example/a:1"]),
        (
            COPY_DESCRIPTION,
            &["package", "copy", "--description", "--to", "r.example", "r.example/a:1"],
        ),
        (CREATE_L, &["package", "create", "-l", "fast", "dir"]),
        (PUSH_C, &["package", "push", "-c", "-i", "r.example/a:1", "a.tar.xz"]),
    ];

    fn parse(argv: &[&str]) -> clap::ArgMatches {
        let mut full = vec!["ocx"];
        full.extend_from_slice(argv);
        crate::app::Cli::command()
            .try_get_matches_from(&full)
            .unwrap_or_else(|error| panic!("`{}` must parse: {error}", full.join(" ")))
    }

    /// Every flag row still parses under its old spelling, is reported as used
    /// by exactly that row, and the new spelling parses and reports nothing.
    ///
    /// The table is checked against [`FLAGS`] first, so a row added without a
    /// command line here reds rather than going unexercised.
    ///
    /// Mutation: delete any row's hidden argument (its parse reds), or misspell
    /// its id (detection reds).
    #[test]
    fn every_renamed_flag_still_parses_and_is_detected() {
        let exercised: Vec<RenamedFlag> = FLAG_ROW_ARGV.iter().map(|(row, _)| *row).collect();
        assert_eq!(FLAGS, exercised, "every flag row needs a command line here");

        for (row, argv) in FLAG_ROW_ARGV {
            assert_eq!(renamed_flags_used(&parse(argv)), [*row], "`ocx {}`", row.old_text());

            let (old, new) = (row.old.to_string(), row.new.to_string());
            let renamed: Vec<&str> = argv
                .iter()
                .map(|word| if *word == old { new.as_str() } else { word })
                .collect();
            assert!(
                renamed_flags_used(&parse(&renamed)).is_empty(),
                "`ocx {}` is the current spelling and must not warn",
                row.new_text()
            );
        }
    }

    /// The old spelling of a value flag conflicts with the new one, so a script
    /// passing both is refused rather than silently getting one of them.
    #[test]
    fn a_renamed_value_flag_conflicts_with_its_replacement() {
        let both = ["package", "create", "-l", "fast", "--compression-level", "best", "dir"];
        let mut full = vec!["ocx"];
        full.extend_from_slice(&both);
        assert!(crate::app::Cli::command().try_get_matches_from(full).is_err());
    }
}
