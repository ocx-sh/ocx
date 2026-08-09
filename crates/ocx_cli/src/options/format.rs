// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::ValueEnum;

/// How stdout reports are rendered.
///
/// Flatten into a command with `#[clap(flatten)]` to add `--format` plus its
/// `--json` shorthand. The two are two spellings of one value, so they use
/// POSIX last-wins semantics in both directions (`overrides_with`, same idiom
/// as [`super::Pull`] / [`super::BinScan`]): combining them is not an error,
/// and either can override the other. That keeps plain reachable when
/// `--json` arrives from outside the command line — a shell alias or wrapper
/// script — which a `conflicts_with` pair would make unexpressible. Resolve
/// with [`Format::mode`] — never read the flags individually.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Format {
    /// Output format for stdout reports: `plain` (default) or `json`.
    ///
    /// Applies to every command; there is no per-command `--format`. The
    /// `--shell[=NAME]` output of `env` / `package env` is unaffected.
    #[clap(long, value_enum, value_name = "FORMAT", overrides_with = "json")]
    format: Option<FormatMode>,

    /// Shorthand for `--format json`.
    ///
    /// Last one wins if combined with `--format`.
    #[clap(long, overrides_with = "format")]
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
    /// `--json` yields `Json`; `--format` yields whatever it names. POSIX
    /// last-wins with both — `overrides_with` guarantees clap has already
    /// dropped the losing occurrence, so at most one is set here.
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

    /// POSIX last-wins in both directions when the two spellings disagree.
    /// The `--json … --format plain` case is the one that matters: it keeps
    /// plain reachable when `--json` is injected by an alias or wrapper.
    #[test]
    fn last_wins() {
        assert_eq!(
            mode(&["--format", "plain", "--json"]),
            FormatMode::Json,
            "--json wins when last"
        );
        assert_eq!(
            mode(&["--json", "--format", "plain"]),
            FormatMode::Plain,
            "--format wins when last"
        );
        assert_eq!(mode(&["--json", "--format", "json"]), FormatMode::Json);
        assert_eq!(mode(&["--format", "json", "--json"]), FormatMode::Json);
    }

    /// `mode`'s doc claims clap drops the losing occurrence, so at most one
    /// field is set. Assert it directly — `mode` reads `format` first, so a
    /// stale `json: true` would stay invisible through the public path.
    #[test]
    fn the_losing_occurrence_is_dropped() {
        let loser_json = Harness::try_parse_from(["harness", "--json", "--format", "plain"])
            .expect("parse")
            .format;
        assert!(!loser_json.json, "an overridden --json must not stay set");
        assert_eq!(loser_json.format, Some(FormatMode::Plain));

        let loser_format = Harness::try_parse_from(["harness", "--format", "plain", "--json"])
            .expect("parse")
            .format;
        assert!(loser_format.json);
        assert_eq!(loser_format.format, None, "an overridden --format must not stay set");
    }
}
