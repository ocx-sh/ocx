// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::ValueEnum;

/// How stdout reports are rendered.
///
/// Flatten into a command with `#[clap(flatten)]` to add `--format` plus its
/// `--json` shorthand. The two are `conflicts_with` rather than last-wins
/// (unlike [`super::Pull`] / [`super::BinScan`]): they are not an on/off pair
/// but two spellings of one value, so a combination that could disagree
/// (`--format plain --json`) is a usage error instead of a silent winner.
/// Resolve with [`Format::mode`] — never read the flags individually.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Format {
    /// Output format for stdout reports: `plain` (default) or `json`.
    ///
    /// Applies to every command; there is no per-command `--format`. The
    /// `--shell[=NAME]` output of `env` / `package env` is unaffected.
    #[clap(long, value_enum, value_name = "FORMAT")]
    format: Option<FormatMode>,

    /// Shorthand for `--format json`.
    ///
    /// Conflicts with `--format`; pass one or the other.
    #[clap(long, conflicts_with = "format")]
    json: bool,
}

/// Resolved output format for stdout reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum FormatMode {
    Json,
    #[default]
    Plain,
}

impl Format {
    /// Resolves the flags to a [`FormatMode`]. Neither flag yields `Plain`;
    /// `--json` yields `Json`; `--format` yields whatever it names. The two
    /// cannot both be present — clap rejects that at parse time.
    pub fn mode(&self) -> FormatMode {
        self.format
            .or(self.json.then_some(FormatMode::Json))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        format: Format,
    }

    fn mode(args: &[&str]) -> FormatMode {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").format.mode()
    }

    /// Neither flag → `Plain`.
    #[test]
    fn no_flags_yield_plain() {
        assert_eq!(mode(&[]), FormatMode::Plain);
    }

    /// Both spellings of JSON resolve identically.
    #[test]
    fn json_flag_is_an_alias_for_format_json() {
        assert_eq!(mode(&["--json"]), FormatMode::Json);
        assert_eq!(mode(&["--format", "json"]), FormatMode::Json);
    }

    /// `--format plain` stays plain, and is not overridden by the default.
    #[test]
    fn explicit_plain_is_honored() {
        assert_eq!(mode(&["--format", "plain"]), FormatMode::Plain);
    }

    /// The two spellings cannot disagree — clap rejects the combination.
    #[test]
    fn json_conflicts_with_format() {
        for args in [["--json", "--format", "json"], ["--json", "--format", "plain"]] {
            let mut argv = vec!["harness"];
            argv.extend_from_slice(&args);
            assert!(Harness::try_parse_from(argv).is_err(), "{args:?} must be rejected");
        }
    }
}
