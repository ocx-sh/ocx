// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::lazy;

/// The `--lazy-report` tier of the lazy-loading resolution ladder.
///
/// **Its one consumer is `ocx launcher shim`** — the hidden verb a generated
/// shim execs on first invocation. Do **not** flatten it into `ocx env`,
/// `run`, `pull`, `package env`, `package exec` or `package which`: those
/// compose, and the download this setting describes happens later, in a
/// *different process*. The shim body is a byte-exact golden carrying no
/// report token, and `OCX_LAZY_REPORT` is deliberately not forwarded as
/// child config, so a compose-time value has no route to the process that
/// would render the progress. It was briefly on all six and could not have
/// worked on any of them.
///
/// The sibling of [`super::LazyMode`], and a second concrete struct for the
/// same reason `lazy::LazyReportLadder` is: two unrelated vocabularies over
/// one shape is incidental similarity, not shared logic. Resolve through
/// [`LazyReport::mode`] — never read the field at a call site.
///
/// What [`LazyReport::mode`] returns is the ladder's **top tier**, not the
/// answer: `None` means "the flag was absent". Feed it to
/// [`lazy::LazyReportLadder::cli`] and call `resolve()`; the floor lives
/// there and nowhere else.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct LazyReport {
    /// Show progress while a deferred tool downloads on first use.
    ///
    /// `silent` opens no progress channel at all. `progress` renders progress
    /// on the controlling terminal; where no terminal is reachable - a
    /// container build, a CI runner, anything detached - it degrades to
    /// silent rather than failing. Errors go to stderr either way.
    ///
    /// Only affects a tool composed with `--lazy-mode always`; an eagerly
    /// composed tool has nothing to defer.
    ///
    /// When omitted, the value is read from `ocx.toml` (the package entry
    /// first, then the top-level `lazy-report` key), then from the
    /// `OCX_LAZY_REPORT` environment variable, and finally defaults to
    /// `silent`. Passing the flag overrides all of them. There is no
    /// per-group setting: unlike `lazy-mode`, this one is resolved when the
    /// download happens rather than when the tool is composed, and no group
    /// is in scope by then.
    ///
    /// See https://ocx.sh/docs/reference/command-line#arg-lazy-report for the
    /// full resolution order.
    #[clap(long = "lazy-report", value_enum, value_name = "MODE")]
    lazy_report: Option<lazy::LazyReport>,
}

impl LazyReport {
    /// Resolves `--lazy-report` to the CLI tier of
    /// [`lazy::LazyReportLadder`].
    ///
    /// `None` means the flag was absent, so the tier is **inherited** from
    /// the next-less-specific one — it never means
    /// [`lazy::LazyReport::Silent`]. Same rationale as
    /// [`super::LazyMode::mode`].
    pub fn mode(&self) -> Option<lazy::LazyReport> {
        self.lazy_report
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::*;

    /// The seven env-composing commands the flag is forbidden on, each as
    /// (argv path, trailing operands the command requires).
    const COMPOSING_COMMANDS: [(&[&str], &[&str]); 7] = [
        (&["env"], &[]),
        (&["exec"], &["--", "true"]),
        (&["pull"], &[]),
        (&["direnv", "export"], &[]),
        (&["package", "env"], &["cmake"]),
        (&["package", "exec"], &["cmake", "--", "true"]),
        (&["package", "which"], &["cmake"]),
    ];

    fn argv(path: &[&str], flag: &[&str], operands: &[&str]) -> Vec<String> {
        std::iter::once("ocx")
            .chain(path.iter().copied())
            .chain(flag.iter().copied())
            .chain(operands.iter().copied())
            .map(str::to_string)
            .collect()
    }

    fn parse(argv: &[String]) -> Result<crate::app::Cli, clap::Error> {
        crate::app::Cli::try_parse_from(argv)
    }

    // ── The resolver ─────────────────────────────────────────────────────────

    /// `ocx launcher shim` flattens `LazyReport` and exercises the real ladder
    /// end to end (`command/launcher/shim.rs`). This local harness pins the
    /// resolver on its own, so a precedence regression is attributed here
    /// rather than surfacing as a failure in the consuming command's tests.
    #[derive(clap::Parser, Debug)]
    struct Harness {
        #[clap(flatten)]
        lazy: LazyReport,
    }

    /// Reads the CLI tier through the resolver, which is what the consumer
    /// calls.
    fn resolved(args: &[&str]) -> Option<lazy::LazyReport> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv)
            .expect("valid invocation parses")
            .lazy
            .mode()
    }

    /// The resolver hands back the parsed value unchanged.
    #[test]
    fn mode_returns_the_parsed_value_verbatim() {
        assert_eq!(
            resolved(&["--lazy-report", "progress"]),
            Some(lazy::LazyReport::Progress)
        );
        assert_eq!(resolved(&["--lazy-report", "silent"]), Some(lazy::LazyReport::Silent));
    }

    /// An omitted flag resolves to `None`: the tier is absent, so the ladder
    /// goes on to ask `ocx.toml` and `OCX_LAZY_REPORT`.
    #[test]
    fn mode_returns_none_for_an_omitted_flag() {
        assert_eq!(
            resolved(&[]),
            None,
            "an omitted --lazy-report must leave the CLI tier absent"
        );
    }

    /// Absence and an explicit `silent` are different answers, and the resolver
    /// must keep them apart. Same shape as [`super::super::LazyMode`]'s guard:
    /// a body of `self.lazy_report.or(Some(LazyReport::Silent))` passes every
    /// other test here while collapsing the CLI tier onto the ladder's floor.
    #[test]
    fn mode_never_substitutes_the_ladder_floor() {
        assert_ne!(
            resolved(&[]),
            Some(lazy::LazyReport::Silent),
            "an omitted --lazy-report must not resolve to the floor; absence means inherit"
        );
        // The floor is still reachable, just only by asking for it.
        assert_eq!(
            resolved(&["--lazy-report", "silent"]),
            Some(lazy::LazyReport::Silent),
            "an explicit --lazy-report silent must resolve to the floor"
        );
    }

    /// No env-composing command accepts `--lazy-report`.
    ///
    /// The struct's doc comment states the prohibition; this makes it
    /// executable. The setting governs what a *materialization* renders, and
    /// materialization happens inside `ocx launcher shim` — a separate process
    /// spawned long after a composing command has exec'd away — so a value
    /// given here could never reach the download it describes.
    ///
    /// `UnknownArgument` rather than a bare `is_err()`: an exit shared by two
    /// failure modes is evidence for neither. Each row also parses the same
    /// invocation with `--lazy-mode always`, which is a positive control on the
    /// same command in the same parser — so a rejection below cannot come from
    /// a command that rejects everything.
    #[test]
    fn no_env_composing_command_accepts_the_flag() {
        for (path, operands) in COMPOSING_COMMANDS {
            parse(&argv(path, &["--lazy-mode", "always"], operands))
                .unwrap_or_else(|error| panic!("`ocx {} --lazy-mode always` must parse: {error}", path.join(" ")));

            let error = parse(&argv(path, &["--lazy-report", "progress"], operands))
                .err()
                .unwrap_or_else(|| panic!("`ocx {}` must not accept --lazy-report", path.join(" ")));
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "`ocx {}` must reject --lazy-report as an unknown argument",
                path.join(" ")
            );
        }
    }

    /// `--lazy-report` is declared on **exactly one** command in the whole
    /// tree: `ocx launcher shim`.
    ///
    /// The sibling of the negative test below, and the half that catches the
    /// other failure: a flag removed from the six but never given to the verb
    /// that consumes it satisfies every "not here" assertion while leaving the
    /// ladder's CLI tier with no producer at all — the settable-and-unreadable
    /// defect one tier up. Naming the exact site, rather than "at least one",
    /// also reds if a future command picks the flag up by accident.
    #[test]
    fn the_flag_is_declared_on_exactly_the_shim_verb() {
        fn walk(command: &clap::Command, path: &str, sites: &mut Vec<String>) {
            let here = if path.is_empty() {
                command.get_name().to_string()
            } else {
                format!("{path} {}", command.get_name())
            };
            if command
                .get_arguments()
                .any(|argument| argument.get_long() == Some("lazy-report"))
            {
                sites.push(here.clone());
            }
            for subcommand in command.get_subcommands() {
                walk(subcommand, &here, sites);
            }
        }

        let mut sites = Vec::new();
        walk(&crate::app::Cli::command(), "", &mut sites);
        sites.sort();
        assert_eq!(
            sites,
            vec!["ocx launcher shim".to_string()],
            "--lazy-report belongs to the process that performs the download, and to no other"
        );
    }

    /// Nowhere in the whole command tree does an env-composing command declare
    /// `--lazy-report`.
    ///
    /// Scoped to the seven rather than asserting the flag is absent everywhere:
    /// `ocx launcher shim` is contracted to gain it, and a test that reds on
    /// that would be pinning the wrong thing. The traversal asserts it reached
    /// all seven via `--lazy-mode`, so an empty `--lazy-report` result cannot
    /// come from a walk that visited nothing.
    #[test]
    fn the_command_tree_declares_the_flag_on_no_composing_command() {
        fn walk(command: &clap::Command, path: &str, longs: &mut Vec<(String, &'static str)>) {
            let here = if path.is_empty() {
                command.get_name().to_string()
            } else {
                format!("{path} {}", command.get_name())
            };
            for argument in command.get_arguments() {
                match argument.get_long() {
                    Some("lazy-report") => longs.push((here.clone(), "lazy-report")),
                    Some("lazy-mode") => longs.push((here.clone(), "lazy-mode")),
                    _ => {}
                }
            }
            for subcommand in command.get_subcommands() {
                walk(subcommand, &here, longs);
            }
        }

        let mut found = Vec::new();
        walk(&crate::app::Cli::command(), "", &mut found);

        let composing: Vec<String> = COMPOSING_COMMANDS
            .iter()
            .map(|(path, _)| format!("ocx {}", path.join(" ")))
            .collect();

        let mut modes: Vec<String> = found
            .iter()
            .filter(|(_, long)| *long == "lazy-mode")
            .map(|(where_, _)| where_.clone())
            .collect();
        modes.sort();
        // The deprecated `ocx run` reuses `ToolchainExec` wholesale, so it
        // necessarily carries `--lazy-mode` too — one composing command with
        // two spellings, not an eighth. Delete this line with the rest of
        // `command::deprecated` in 0.7.
        let mut expected = composing.clone();
        expected.push("ocx run".to_string());
        expected.sort();
        assert_eq!(
            modes, expected,
            "the walk must reach exactly the seven composing commands \
             (plus the deprecated `run` spelling of `exec`, removed in 0.7)"
        );

        let offenders: Vec<&String> = found
            .iter()
            .filter(|(where_, long)| *long == "lazy-report" && composing.contains(where_))
            .map(|(where_, _)| where_)
            .collect();
        assert!(
            offenders.is_empty(),
            "--lazy-report is unimplementable on a composing command; found it on {offenders:?}"
        );
    }
}
