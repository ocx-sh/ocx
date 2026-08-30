// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::lazy;

/// The `--lazy-mode` tier of the lazy-loading resolution ladder.
///
/// Flatten into a command with `#[clap(flatten)]` to add `--lazy-mode <MODE>`.
/// Deliberately *not* a `--X`/`--no-X` pair (the `options::Pull` /
/// `options::BinScan` shape): a paired toggle can only express a closed two-
/// or three-valued set, and this mode is an open-ended strategy enum. Resolve
/// through [`LazyMode::mode`] — never read the field at a call site.
///
/// What [`LazyMode::mode`] returns is the ladder's **top tier**, not the
/// answer: `None` means "the flag was absent", which is what lets `ocx.toml`
/// and `OCX_LAZY_MODE` speak. Feed it to [`lazy::LazyModeLadder::cli`] and
/// call `resolve()`; the floor lives there and nowhere else.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct LazyMode {
    /// Control when a tool's content downloads: now, or on first use.
    ///
    /// `never` composes eagerly, so a tool's content is materialized before
    /// the tool reaches `PATH`. `always` composes a shim instead: the tool's
    /// declared names are on `PATH` immediately, and its content downloads
    /// the first time one of those names runs.
    ///
    /// When omitted, the value is read from `ocx.toml` (the package entry
    /// first, then the group, then the top-level `lazy-mode` key), then from
    /// the `OCX_LAZY_MODE` environment variable, and finally defaults to
    /// `never`. Passing the flag overrides all of them.
    ///
    /// See https://ocx.sh/docs/reference/command-line#arg-lazy-mode for the
    /// full resolution order.
    #[clap(long = "lazy-mode", value_enum, value_name = "MODE")]
    lazy_mode: Option<lazy::LazyMode>,
}

impl LazyMode {
    /// Resolves `--lazy-mode` to the CLI tier of [`lazy::LazyModeLadder`].
    ///
    /// `None` means the flag was absent, so the tier is **inherited** from
    /// the next-less-specific one — it never means [`lazy::LazyMode::Never`].
    /// Collapsing absence into the floor here would make `--lazy-mode`
    /// silently outrank every `ocx.toml` tier on every invocation.
    pub fn mode(&self) -> Option<lazy::LazyMode> {
        self.lazy_mode
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::*;

    // ── The published command matrix (C-021) ─────────────────────────────────

    /// The seven env-composing commands `--lazy-mode` is accepted by, each as
    /// (argv path, trailing operands the command requires). Composition or
    /// pre-warming, as distinct from installing into the symlink namespace.
    ///
    /// `ocx direnv export` is one of them: the `ocx.toml` tiers make a tool
    /// deferred with no flag typed at all, so a direnv-composed environment
    /// that ignored `lazy-mode` would differ from the `ocx env` one for the
    /// same project.
    const COMPOSING_COMMANDS: [(&[&str], &[&str]); 7] = [
        (&["env"], &[]),
        (&["exec"], &["--", "true"]),
        (&["pull"], &[]),
        (&["direnv", "export"], &[]),
        (&["package", "env"], &["cmake"]),
        (&["package", "exec"], &["cmake", "--", "true"]),
        (&["package", "which"], &["cmake"]),
    ];

    /// The two commands that write the `candidates/` + `current` symlink
    /// namespace. A symlink must never point at a shim dir, so these always
    /// materialize and never accept the flag.
    const MATERIALIZING_COMMANDS: [(&[&str], &[&str]); 2] = [
        (&["package", "install"], &["cmake"]),
        (&["package", "select"], &["cmake"]),
    ];

    /// Builds a full `ocx` argv: the command path, then the flag under test,
    /// then whatever operands the command requires. The flag goes *before* the
    /// operands because that is the invocation the project's flag ordering
    /// convention publishes.
    fn argv(path: &[&str], flag: &[&str], operands: &[&str]) -> Vec<String> {
        std::iter::once("ocx")
            .chain(path.iter().copied())
            .chain(flag.iter().copied())
            .chain(operands.iter().copied())
            .map(str::to_string)
            .collect()
    }

    /// Runs argv through the real `ocx` parser — the same `Cli` definition the
    /// binary builds, not a stand-in.
    fn parse(argv: &[String]) -> Result<crate::app::Cli, clap::Error> {
        crate::app::Cli::try_parse_from(argv)
    }

    /// Descends `Cli::command()` along a subcommand path. Panics when a segment
    /// is missing, so a renamed command fails loudly here instead of silently
    /// making every assertion below vacuous.
    fn command_at(path: &[&str]) -> clap::Command {
        let mut command = crate::app::Cli::command();
        for segment in path {
            command = command
                .find_subcommand(segment)
                .unwrap_or_else(|| panic!("`ocx {}` must exist in the command tree", path.join(" ")))
                .clone();
        }
        command
    }

    /// The `--lazy-mode` argument as the given command declares it.
    fn lazy_mode_arg(path: &[&str]) -> clap::Arg {
        command_at(path)
            .get_arguments()
            .find(|arg| arg.get_long() == Some("lazy-mode"))
            .unwrap_or_else(|| panic!("`ocx {}` must declare --lazy-mode", path.join(" ")))
            .clone()
    }

    // ── Value grammar ────────────────────────────────────────────────────────

    #[derive(clap::Parser, Debug)]
    struct Harness {
        #[clap(flatten)]
        lazy: LazyMode,
    }

    /// Reads the parsed CLI tier straight off the field.
    ///
    /// Deliberately the field and not [`LazyMode::mode`]: the tests below pin
    /// what clap *parses*, independently of what the resolver does with it. The
    /// resolver has its own tests further down, and `resolved` is the helper
    /// that goes through it.
    fn tier(args: &[&str]) -> Option<lazy::LazyMode> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv)
            .expect("valid invocation parses")
            .lazy
            .lazy_mode
    }

    fn parse_harness(args: &[&str]) -> Result<Harness, clap::Error> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv)
    }

    /// An omitted flag leaves the CLI tier **absent**, which is what lets the
    /// `ocx.toml` and `OCX_LAZY_MODE` tiers speak. A default of `never` here
    /// would make the flag outrank every one of them on every invocation.
    #[test]
    fn an_omitted_flag_leaves_the_cli_tier_absent() {
        assert_eq!(
            tier(&[]),
            None,
            "an omitted --lazy-mode must be absent, never the ladder's floor"
        );
    }

    /// Both published values parse to their variant verbatim.
    #[test]
    fn each_published_value_parses_to_its_variant() {
        assert_eq!(tier(&["--lazy-mode", "never"]), Some(lazy::LazyMode::Never));
        assert_eq!(tier(&["--lazy-mode", "always"]), Some(lazy::LazyMode::Always));
    }

    /// Space- and `=`-separated forms are the same invocation.
    #[test]
    fn the_value_may_be_space_or_equals_separated() {
        assert_eq!(tier(&["--lazy-mode=always"]), Some(lazy::LazyMode::Always));
        assert_eq!(tier(&["--lazy-mode", "always"]), Some(lazy::LazyMode::Always));
    }

    /// The value is case-sensitive: only the lowercase wire spelling is
    /// accepted, so a user typing `--lazy-mode Always` is told so rather than
    /// silently ignored. Case folding belongs to the environment tier alone.
    #[test]
    fn the_value_is_case_sensitive() {
        for value in ["Always", "ALWAYS", "Never", "NEVER"] {
            assert!(
                parse_harness(&["--lazy-mode", value]).is_err(),
                "--lazy-mode {value} must be rejected; only the lowercase spelling is published"
            );
        }
        // Positive control on the same parser: the lowercase spellings do parse,
        // so the rejections above cannot come from a flag that never existed.
        assert_eq!(tier(&["--lazy-mode", "always"]), Some(lazy::LazyMode::Always));
        assert_eq!(tier(&["--lazy-mode", "never"]), Some(lazy::LazyMode::Never));
    }

    /// A value outside the published set is an invalid-value error, not a
    /// silently ignored argument.
    #[test]
    fn an_unpublished_value_is_an_invalid_value_error() {
        let error = parse_harness(&["--lazy-mode", "eager"]).expect_err("'eager' is not a published value");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    }

    /// The flag takes a value; bare `--lazy-mode` is a usage error.
    #[test]
    fn the_flag_requires_a_value() {
        let error = parse_harness(&["--lazy-mode"]).expect_err("--lazy-mode must not be a bare toggle");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        // Positive control: the same flag with a value parses.
        assert_eq!(tier(&["--lazy-mode", "never"]), Some(lazy::LazyMode::Never));
    }

    // ── The resolver ─────────────────────────────────────────────────────────

    /// Reads the CLI tier through the resolver, which is what every consumer
    /// calls. The sibling of [`tier`], which reads the parsed field directly;
    /// the two must agree, and the tests below are what say so.
    fn resolved(args: &[&str]) -> Option<lazy::LazyMode> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv)
            .expect("valid invocation parses")
            .lazy
            .mode()
    }

    /// The resolver hands back the parsed value unchanged — no mapping, no
    /// normalization, no substitution.
    #[test]
    fn mode_returns_the_parsed_value_verbatim() {
        assert_eq!(resolved(&["--lazy-mode", "always"]), Some(lazy::LazyMode::Always));
        assert_eq!(resolved(&["--lazy-mode", "never"]), Some(lazy::LazyMode::Never));
    }

    /// An omitted flag resolves to `None`: the tier is absent, so the ladder
    /// goes on to ask `ocx.toml` and `OCX_LAZY_MODE`.
    #[test]
    fn mode_returns_none_for_an_omitted_flag() {
        assert_eq!(
            resolved(&[]),
            None,
            "an omitted --lazy-mode must leave the CLI tier absent"
        );
    }

    /// Absence and an explicit `never` are different answers, and the resolver
    /// must keep them apart.
    ///
    /// A body of `self.lazy_mode.or(Some(LazyMode::Never))` passes every other
    /// test in this file — the flag still parses, the field still holds the
    /// right value — while silently collapsing the CLI tier onto the ladder's
    /// floor, so `ocx.toml`, the group, the toolchain and the environment would
    /// never be consulted again. This is the assertion that reds on it.
    #[test]
    fn mode_never_substitutes_the_ladder_floor() {
        assert_ne!(
            resolved(&[]),
            Some(lazy::LazyMode::Never),
            "an omitted --lazy-mode must not resolve to the floor; absence means inherit"
        );
        // The floor is still reachable, just only by asking for it — otherwise
        // the assertion above would also hold for a resolver that can never
        // return `Never` at all.
        assert_eq!(
            resolved(&["--lazy-mode", "never"]),
            Some(lazy::LazyMode::Never),
            "an explicit --lazy-mode never must resolve to the floor"
        );
    }

    // ── Command matrix ───────────────────────────────────────────────────────

    /// Every env-composing command accepts the flag, proven against the real
    /// parser rather than a help render: help absence is a weaker claim than
    /// parser absence, and its presence is a weaker claim than a parse.
    #[test]
    fn every_env_composing_command_accepts_the_flag() {
        for (path, operands) in COMPOSING_COMMANDS {
            // Control first: the invocation without the flag must be valid, or a
            // failure below would be attributable to the operands instead.
            parse(&argv(path, &[], operands))
                .unwrap_or_else(|error| panic!("`ocx {}` base invocation must parse: {error}", path.join(" ")));

            parse(&argv(path, &["--lazy-mode", "always"], operands))
                .unwrap_or_else(|error| panic!("`ocx {} --lazy-mode always` must parse: {error}", path.join(" ")));
        }
    }

    /// `install` and `select` never accept the flag. `UnknownArgument` — not
    /// merely "an error" — is the assertion that separates "there is no such
    /// flag here" from "this invocation was rejected for some other reason".
    #[test]
    fn install_and_select_reject_the_flag() {
        for (path, operands) in MATERIALIZING_COMMANDS {
            parse(&argv(path, &[], operands))
                .unwrap_or_else(|error| panic!("`ocx {}` base invocation must parse: {error}", path.join(" ")));

            let error = parse(&argv(path, &["--lazy-mode", "always"], operands))
                .err()
                .unwrap_or_else(|| panic!("`ocx {}` must not accept --lazy-mode", path.join(" ")));
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "`ocx {}` must reject --lazy-mode as an unknown argument",
                path.join(" ")
            );
        }
    }

    /// One spelling, six commands: long form only, no short form anywhere.
    #[test]
    fn the_flag_is_spelled_identically_on_every_composing_command() {
        for (path, _) in COMPOSING_COMMANDS {
            let arg = lazy_mode_arg(path);
            assert_eq!(arg.get_long(), Some("lazy-mode"), "on `ocx {}`", path.join(" "));
            assert_eq!(
                arg.get_short(),
                None,
                "`ocx {}` must not give --lazy-mode a short form",
                path.join(" ")
            );
        }
    }

    /// The usage string is `--lazy-mode <MODE>` on all six, and the flag is
    /// never required.
    #[test]
    fn the_flag_takes_one_optional_mode_value_on_every_composing_command() {
        for (path, _) in COMPOSING_COMMANDS {
            let arg = lazy_mode_arg(path);
            let value_names: Vec<String> = arg
                .get_value_names()
                .unwrap_or_default()
                .iter()
                .map(ToString::to_string)
                .collect();
            assert_eq!(value_names, ["MODE"], "on `ocx {}`", path.join(" "));
            assert!(
                !arg.is_required_set(),
                "`ocx {}` must not make --lazy-mode required",
                path.join(" ")
            );
        }
    }

    /// The published value set is exactly `never`, `always` — in that order, so
    /// `--help` reads the same on every command.
    #[test]
    fn the_flag_offers_exactly_never_and_always_on_every_composing_command() {
        for (path, _) in COMPOSING_COMMANDS {
            let values: Vec<String> = lazy_mode_arg(path)
                .get_possible_values()
                .iter()
                .map(|value| value.get_name().to_string())
                .collect();
            assert_eq!(values, ["never", "always"], "on `ocx {}`", path.join(" "));
        }
    }

    /// The flag is declared before every positional argument, per the project's
    /// flags-before-positional-arguments convention. `get_arguments()` yields
    /// declaration order, so the comparison is on the source ordering that
    /// `--help` and the struct both reflect.
    #[test]
    fn the_flag_is_declared_before_every_positional() {
        let mut commands_with_positionals = 0;
        for (path, _) in COMPOSING_COMMANDS {
            let command = command_at(path);
            let arguments: Vec<&clap::Arg> = command.get_arguments().collect();
            let flag_at = arguments
                .iter()
                .position(|arg| arg.get_long() == Some("lazy-mode"))
                .unwrap_or_else(|| panic!("`ocx {}` must declare --lazy-mode", path.join(" ")));
            let Some(positional_at) = arguments.iter().position(|arg| arg.is_positional()) else {
                continue;
            };
            commands_with_positionals += 1;
            assert!(
                flag_at < positional_at,
                "`ocx {}` declares --lazy-mode after its first positional argument",
                path.join(" ")
            );
        }
        assert!(
            commands_with_positionals >= 4,
            "at least four of the six composing commands take positionals; \
             saw {commands_with_positionals}, so this guard is not measuring what it claims"
        );
    }
}
