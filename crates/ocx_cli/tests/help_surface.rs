// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Gate: what a command's `--help` actually renders.
//!
//! clap takes a subcommand's `about` / `long_about` from the **variant** that
//! holds the args struct, and a `///` on the struct itself is orphaned rustdoc
//! nobody can reach from a terminal. Three contracts on this branch were
//! authored on the struct — `ocx shell state`'s entire contract, `ocx self
//! setup`'s two sentences answering "why is nothing on my PATH", and `ocx
//! pull`'s scoping clause, whose absence turned an inaccurate about line into a
//! statement no correction could reach. Nothing tied the authored text to the
//! rendered text, so all three passed review.
//!
//! This ties them. Every assertion below runs against the string clap renders,
//! never against a source file: a phrase moved back onto a struct doc, or
//! deleted, fails here even though it is still spelled somewhere in the crate.
//!
//! ponytail: a required-phrase table, not a grammar. It catches the class that
//! actually recurred — a contract sentence going missing from the rendered
//! surface — and costs one line per phrase.

use clap::CommandFactory;
use ocx::app::Cli;

/// The rendered long help of the command at `path`, whitespace-normalised.
///
/// clap hard-wraps to the terminal width, so a phrase that fits on one line
/// here can be split across two on a narrower terminal. Collapsing every
/// whitespace run to a single space makes a needle match the words rather than
/// the layout.
fn long_help(path: &[&str]) -> String {
    let mut command = Cli::command();
    for name in path {
        command = command
            .find_subcommand(name)
            .unwrap_or_else(|| panic!("no such subcommand: `ocx {}`", path.join(" ")))
            .clone();
    }
    normalize(&command.render_long_help().to_string())
}

/// The rendered help of the root command, whitespace-normalised — the listing
/// that carries every subcommand's `about` line.
fn root_help() -> String {
    normalize(&Cli::command().render_help().to_string())
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One required phrase per command: `(subcommand path, phrase)`.
///
/// A phrase is chosen so it can only occur in help text — never a symbol name,
/// never a string this test file could match against itself, since the haystack
/// is the rendered output and not any file on disk.
const REQUIRED: &[(&[&str], &str)] = &[
    // V-17 / V-18 — the five commands that render a link tree and a `bin/`
    // launcher set into the user's checkout must say so, and name the directory.
    (&["add"], "<project>/.ocx/toolchain/"),
    (&["remove"], "<project>/.ocx/toolchain/"),
    (&["lock"], "<project>/.ocx/toolchain/"),
    (&["update"], "<project>/.ocx/toolchain/"),
    (&["pull"], "<project>/.ocx/toolchain/"),
    // V-17 — `pull` renders; the about line has to lead with that, not deny it.
    (&["pull"], "render the project toolchain"),
    // V-20 — `ocx shell state`'s contract: read-only, the repair gesture, the
    // exit rule.
    (&["shell", "state"], "unset __OCX_ENV_STATE"),
    (&["shell", "state"], "never writes a consent stamp"),
    (&["shell", "state"], "Exits 0 in every reportable state"),
    // V-20 — `ocx self setup`'s two sentences answering "why is nothing on my
    // PATH", plus the non-fatal session-PATH rule.
    (&["self", "setup"], "already open see nothing until they are restarted"),
    (&["self", "setup"], "only after the next login"),
    (&["self", "setup"], "reported and warned about, never fatal"),
];

#[test]
fn every_listed_command_renders_its_required_phrase() {
    assert!(
        !REQUIRED.is_empty(),
        "an empty table would pass without checking anything"
    );
    let mut missing = Vec::new();
    for (path, phrase) in REQUIRED {
        let rendered = long_help(path);
        if !rendered.contains(&normalize(phrase)) {
            missing.push(format!("`ocx {} --help` does not render: {phrase}", path.join(" ")));
        }
    }
    assert!(
        missing.is_empty(),
        "help text is authored where clap does not render it, or was deleted:\n  {}",
        missing.join("\n  ")
    );
}

/// V-17's own half: the about line is what `ocx -h` prints for `pull`, and it
/// used to state the opposite of what the command does.
///
/// Asserted against the **root** listing rather than `pull --help`, because the
/// about line is the only part of `pull`'s help that reaches a user who has not
/// asked about `pull` yet.
#[test]
fn the_root_listing_does_not_claim_pull_creates_no_symlinks() {
    let rendered = root_help();
    assert!(
        rendered.contains("render the project toolchain"),
        "the root listing must describe what `pull` does; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("without creating symlinks"),
        "`pull` renders a link tree, so the root listing must not claim otherwise; got:\n{rendered}"
    );
}

/// `--handoff` is `hide = true`: machine surface `ocx self update`'s post-swap
/// hand-off spawns, never something a user should be offered. `hide = true`
/// is a claim nothing checks unless something greps the rendered surface for
/// it — this is that check.
#[test]
fn self_setup_help_never_renders_the_hidden_handoff_flag() {
    let rendered = long_help(&["self", "setup"]);
    assert!(
        !rendered.contains("--handoff"),
        "`--handoff` must stay hidden from `ocx self setup --help`; got:\n{rendered}"
    );
}
