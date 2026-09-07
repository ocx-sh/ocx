// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// The `--pinned` / `--no-pinned` tier of the `pinned` resolution ladder.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired flags. The
/// two use POSIX last-wins semantics (`overrides_with`) — combining them is not
/// an error (the git `--[no-]verify` idiom), the same shape as
/// [`super::Pull`] and [`super::BinScan`]. Resolve through [`Pinned::pinned`] —
/// never read either field at a call site.
///
/// # Why a pair, and why it still returns an `Option`
///
/// `pinned` is a **closed two-valued** setting, which is exactly the case
/// [`super::LazyMode`]'s doc excludes itself from ("a paired toggle can only
/// express a closed two- or three-valued set, and this mode is an open-ended
/// strategy enum"). Without `--no-pinned`, a project declaring `pinned = true`
/// in `ocx.toml` could never be overridden back to following the links from the
/// command line.
///
/// The return stays `Option<bool>` because this is a **ladder tier**, not a
/// setting: `None` means "neither flag was given", which is what lets `ocx.toml`
/// and `OCX_TOOLCHAIN_PINNED` speak. Feed it to
/// [`Ladder::cli`](ocx_lib::ladder::Ladder) and call `resolve(PINNED_FLOOR)`;
/// the floor lives there and nowhere else.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Pinned {
    /// Compose digest paths instead of the toolchain links.
    ///
    /// The environment then names the exact packages `ocx.lock` pins right now
    /// and consults no `<group>/<entry>` link.
    ///
    /// When neither flag is given, the value is read from `ocx.toml` (the
    /// `pinned` key), then from the `OCX_TOOLCHAIN_PINNED` environment
    /// variable, and finally defaults to following the links. Passing either
    /// flag overrides all of them.
    ///
    /// See https://ocx.sh/docs/reference/command-line#arg-pinned for the full
    /// resolution order.
    #[clap(long = "pinned", overrides_with = "no_pinned")]
    pinned: bool,

    /// Compose through the toolchain links instead of digest paths.
    ///
    /// The rendered `<group>/<entry>` links are followed, so a later
    /// `ocx update` takes effect with no re-render. This is the default; pass
    /// it to override an `ocx.toml` or `OCX_TOOLCHAIN_PINNED` that asked for
    /// digest paths.
    ///
    /// See https://ocx.sh/docs/reference/command-line#arg-pinned for the full
    /// resolution order.
    #[clap(long = "no-pinned", overrides_with = "pinned")]
    no_pinned: bool,
}

impl Pinned {
    /// Resolves the paired flags to the CLI tier of the `pinned` ladder.
    ///
    /// `None` means neither flag was given, so the tier is **inherited** from
    /// the next-less-specific one — it never means `Some(false)`, which is what
    /// `--no-pinned` is for. Collapsing absence into `false` here would make the
    /// flags' absence silently outrank an `ocx.toml` that asked for pinning.
    ///
    /// POSIX last-wins with both flags — `overrides_with` guarantees at most one
    /// of the two is `true` after parsing.
    #[must_use]
    pub fn pinned(&self) -> Option<bool> {
        match (self.pinned, self.no_pinned) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            (false, false) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::*;

    // ── The published command matrix (C-055) ─────────────────────────────────

    /// The two commands C-055 names, each as (argv path, trailing operands the
    /// command requires).
    ///
    /// `ocx env` and `ocx exec` are the project-toolchain composing emitters
    /// whose output `pinned` selects between (C-066): the digest lane or the
    /// `<group>/<entry>` link lane. Nothing else composes a *toolchain*.
    const PINNED_COMMANDS: [(&[&str], &[&str]); 2] = [(&["env"], &[]), (&["exec"], &["--", "true"])];

    /// The package-tier siblings. A package composition has no toolchain tree
    /// and therefore no link lane to choose, so the flag is meaningless there
    /// and must be an unknown argument rather than an accepted no-op.
    const PACKAGE_TIER_COMMANDS: [(&[&str], &[&str]); 2] = [
        (&["package", "env"], &["cmake"]),
        (&["package", "exec"], &["cmake", "--", "true"]),
    ];

    /// Builds a full `ocx` argv: the command path, then the flags under test,
    /// then whatever operands the command requires. Flags before operands, per
    /// the project's flag-ordering convention.
    fn argv(path: &[&str], flags: &[&str], operands: &[&str]) -> Vec<String> {
        std::iter::once("ocx")
            .chain(path.iter().copied())
            .chain(flags.iter().copied())
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

    /// The named argument as the given command declares it.
    fn flag_arg(path: &[&str], long: &str) -> clap::Arg {
        command_at(path)
            .get_arguments()
            .find(|arg| arg.get_long() == Some(long))
            .unwrap_or_else(|| panic!("`ocx {}` must declare --{long}", path.join(" ")))
            .clone()
    }

    // ── The value grammar (C-055, RUL-60) ────────────────────────────────────

    #[derive(clap::Parser, Debug)]
    struct Harness {
        #[clap(flatten)]
        pinned: Pinned,
    }

    fn parse_harness(args: &[&str]) -> Result<Harness, clap::Error> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv)
    }

    /// Reads the two parsed booleans straight off the fields.
    ///
    /// Deliberately the fields and not [`Pinned::pinned`]: these pin what clap
    /// *parses*, independently of what the resolver does with it. The resolver
    /// has its own cases below, and `resolved` is the helper that goes through
    /// it.
    fn fields(args: &[&str]) -> (bool, bool) {
        let harness = parse_harness(args).expect("valid invocation parses");
        (harness.pinned.pinned, harness.pinned.no_pinned)
    }

    /// Reads the CLI tier through the resolver, which is what every consumer
    /// calls.
    fn resolved(args: &[&str]) -> Option<bool> {
        parse_harness(args).expect("valid invocation parses").pinned.pinned()
    }

    /// C-055 / RUL-60 — an omitted pair leaves the CLI tier **absent**, which
    /// is what lets `ocx.toml`'s `pinned` key and `OCX_TOOLCHAIN_PINNED` speak.
    #[test]
    fn an_omitted_pair_leaves_the_cli_tier_absent() {
        assert_eq!(
            resolved(&[]),
            None,
            "neither flag given must leave the CLI tier absent, never the ladder's floor"
        );
    }

    /// C-055 / RUL-60 — each flag resolves to the value it names.
    #[test]
    fn each_flag_resolves_to_its_own_value() {
        assert_eq!(resolved(&["--pinned"]), Some(true), "--pinned selects the digest lane");
        assert_eq!(
            resolved(&["--no-pinned"]),
            Some(false),
            "--no-pinned selects the link-following lane"
        );
    }

    /// RUL-60 — the pair is POSIX last-wins, not a usage error.
    ///
    /// The git `--[no-]verify` idiom, and the shape `overrides_with` gives.
    /// Both orders, because a one-sided `overrides_with` passes the first and
    /// reds the second.
    #[test]
    fn the_last_flag_wins_in_either_order() {
        assert_eq!(
            resolved(&["--pinned", "--no-pinned"]),
            Some(false),
            "--no-pinned last must win"
        );
        assert_eq!(
            resolved(&["--no-pinned", "--pinned"]),
            Some(true),
            "--pinned last must win"
        );
    }

    /// RUL-60's parse half: `overrides_with` leaves at most one field `true`,
    /// so the resolver never sees the both-set state.
    ///
    /// Separate from the case above because it is what makes that one's answer
    /// well-defined rather than an artefact of which field the resolver reads
    /// first.
    #[test]
    fn overrides_with_leaves_at_most_one_field_set() {
        assert_eq!(fields(&[]), (false, false), "neither flag given");
        assert_eq!(fields(&["--pinned"]), (true, false));
        assert_eq!(fields(&["--no-pinned"]), (false, true));
        assert_eq!(
            fields(&["--pinned", "--no-pinned"]),
            (false, true),
            "the second flag must clear the first, not add to it"
        );
        assert_eq!(fields(&["--no-pinned", "--pinned"]), (true, false));
    }

    /// RUL-60's whole reason for existing: **`--no-pinned` must be able to
    /// override an `ocx.toml` `pinned = true` back to following the links.**
    ///
    /// That is only expressible if absence and an explicit `false` are
    /// different answers. A resolver body of `Some(self.pinned)` — the
    /// single-flag shape a literal reading of the plan would produce — passes
    /// `each_flag_resolves_to_its_own_value` for `--pinned` and every parse
    /// case in this file, while collapsing absence onto `Some(false)` so no
    /// lower tier is ever consulted again. This is the assertion that reds on
    /// it.
    #[test]
    fn absence_is_never_the_following_default() {
        assert_ne!(
            resolved(&[]),
            Some(false),
            "an omitted pair must not resolve to `false`; absence means inherit"
        );
        // `false` is still reachable, just only by asking for it — otherwise
        // the assertion above would also hold for a resolver that can never
        // return `Some(false)` at all.
        assert_eq!(
            resolved(&["--no-pinned"]),
            Some(false),
            "an explicit --no-pinned must resolve to `false`"
        );
    }

    /// The mirror control: absence is not an implicit `--pinned` either.
    #[test]
    fn absence_is_never_an_implicit_pin() {
        assert_ne!(
            resolved(&[]),
            Some(true),
            "an omitted pair must not resolve to `true`; absence means inherit"
        );
        assert_eq!(resolved(&["--pinned"]), Some(true), "an explicit --pinned must pin");
    }

    /// Neither flag takes a value: `--pinned=true` is a usage error, not a
    /// parsed `true`.
    #[test]
    fn neither_flag_takes_a_value() {
        for spelling in ["--pinned=true", "--no-pinned=false"] {
            assert!(
                parse_harness(&[spelling]).is_err(),
                "{spelling} must be rejected; both flags are bare toggles"
            );
        }
        // Positive control on the same parser: the bare spellings do parse.
        assert!(parse_harness(&["--pinned"]).is_ok());
        assert!(parse_harness(&["--no-pinned"]).is_ok());
    }

    // ── The command matrix (C-055, RUL-50) ───────────────────────────────────

    /// C-055 — both commands accept both flags, proven against the real parser
    /// rather than a help render.
    #[test]
    fn every_composing_command_accepts_both_flags() {
        for (path, operands) in PINNED_COMMANDS {
            // Control first: the invocation without the flag must be valid, or
            // a failure below would be attributable to the operands instead.
            parse(&argv(path, &[], operands))
                .unwrap_or_else(|error| panic!("`ocx {}` base invocation must parse: {error}", path.join(" ")));

            for flag in ["--pinned", "--no-pinned"] {
                parse(&argv(path, &[flag], operands))
                    .unwrap_or_else(|error| panic!("`ocx {} {flag}` must parse: {error}", path.join(" ")));
            }
        }
    }

    /// RUL-50 — the pair is accepted on the **global tier** too.
    ///
    /// There is a global `ocx.toml`, so `pinned` has a producer on both tiers
    /// and the flag selects the emitter lane wherever a composition is. Refusing
    /// it under `--global` would be a usage error for a meaningful invocation.
    #[test]
    fn the_global_tier_accepts_both_flags() {
        for (path, operands) in PINNED_COMMANDS {
            for flag in ["--pinned", "--no-pinned"] {
                let argv: Vec<String> = std::iter::once("ocx")
                    .chain(std::iter::once("--global"))
                    .chain(path.iter().copied())
                    .chain(std::iter::once(flag))
                    .chain(operands.iter().copied())
                    .map(str::to_string)
                    .collect();
                parse(&argv)
                    .unwrap_or_else(|error| panic!("`ocx --global {} {flag}` must parse: {error}", path.join(" ")));
            }
        }
    }

    /// The package tier never accepts the pair. `UnknownArgument` — not merely
    /// "an error" — separates "there is no such flag here" from "this
    /// invocation was rejected for some other reason".
    #[test]
    fn the_package_tier_rejects_both_flags() {
        for (path, operands) in PACKAGE_TIER_COMMANDS {
            parse(&argv(path, &[], operands))
                .unwrap_or_else(|error| panic!("`ocx {}` base invocation must parse: {error}", path.join(" ")));

            for flag in ["--pinned", "--no-pinned"] {
                let error = parse(&argv(path, &[flag], operands))
                    .err()
                    .unwrap_or_else(|| panic!("`ocx {}` must not accept {flag}", path.join(" ")));
                assert_eq!(
                    error.kind(),
                    clap::error::ErrorKind::UnknownArgument,
                    "`ocx {}` must reject {flag} as an unknown argument",
                    path.join(" ")
                );
            }
        }
    }

    /// One spelling on both commands: long form only, no short form anywhere.
    #[test]
    fn the_flags_are_spelled_identically_on_every_composing_command() {
        for (path, _) in PINNED_COMMANDS {
            for long in ["pinned", "no-pinned"] {
                let arg = flag_arg(path, long);
                assert_eq!(arg.get_long(), Some(long), "on `ocx {}`", path.join(" "));
                assert_eq!(
                    arg.get_short(),
                    None,
                    "`ocx {}` must not give --{long} a short form",
                    path.join(" ")
                );
                assert!(
                    !arg.is_required_set(),
                    "`ocx {}` must not make --{long} required",
                    path.join(" ")
                );
            }
        }
    }

    /// C-055's ordering half — the pair is declared **before every positional**
    /// on every command that accepts it, per the project's
    /// flags-before-positional-arguments convention.
    ///
    /// `get_arguments()` yields declaration order, so the comparison is on the
    /// source ordering that `--help` and the struct both reflect. `ocx exec`
    /// carries positionals (`names`, `argv`), so the guard is not vacuous; the
    /// count assertion is what says so.
    #[test]
    fn the_flags_are_declared_before_every_positional() {
        let mut commands_with_positionals = 0;
        for (path, _) in PINNED_COMMANDS {
            let command = command_at(path);
            let arguments: Vec<&clap::Arg> = command.get_arguments().collect();
            let Some(positional_at) = arguments.iter().position(|arg| arg.is_positional()) else {
                continue;
            };
            commands_with_positionals += 1;
            for long in ["pinned", "no-pinned"] {
                let flag_at = arguments
                    .iter()
                    .position(|arg| arg.get_long() == Some(long))
                    .unwrap_or_else(|| panic!("`ocx {}` must declare --{long}", path.join(" ")));
                assert!(
                    flag_at < positional_at,
                    "`ocx {}` declares --{long} after its first positional argument",
                    path.join(" ")
                );
            }
        }
        assert!(
            commands_with_positionals >= 1,
            "`ocx exec` takes positionals; saw {commands_with_positionals} commands with any, \
             so this guard is not measuring what it claims"
        );
    }
}
