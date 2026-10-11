// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// [direnv](https://direnv.net) integration for the project toolchain.
///
/// Bare `ocx direnv` is shorthand for `ocx direnv init` — the once-per-project
/// setup that writes a `.envrc`. The generated `.envrc` evaluates
/// `ocx direnv export` on every directory entry.
#[derive(Parser)]
pub struct Direnv {
    #[command(subcommand)]
    command: Option<DirenvCommand>,
}

#[derive(Subcommand)]
enum DirenvCommand {
    /// Write a `.envrc` wiring `ocx direnv export` into direnv.
    ///
    /// Writes the file in the current directory; run `direnv allow` afterwards to activate it. The
    /// generated `.envrc` watches `ocx.toml` and `ocx.lock`, so direnv re-runs the hook whenever
    /// either changes, and the hook itself is `eval "$(ocx direnv export)"`.
    ///
    /// Because `ocx direnv export` always emits bash, the file works for teams with different
    /// interactive shells.
    Init(super::direnv_init::DirenvInit),
    /// Print stateless shell exports for the project toolchain (direnv entry point).
    #[command(long_about = "\
        Print stateless shell exports for the project toolchain (direnv entry point).\n\n\
        Reads the nearest project `ocx.toml` (no home-tier fallback) and its `ocx.lock`, and resolves \
        the selected bindings. It prints a fresh block of bash export lines on every invocation, \
        leaving diffing and caching to direnv. You do not normally type it by hand; the generated \
        `.envrc` evaluates it.\n\n\
        The output is always bash because direnv sources `.envrc` in a bash sub-shell and \
        translates the result to the interactive shell itself.\n\n\
        A package missing from the object store is pulled on a miss (skip that with `--no-pull`). The pull is \
        best-effort and only happens when a registry is reachable, so a missing package never blocks the \
        prompt. Otherwise it is noted on stderr and skipped. A stale lock warns on stderr and its \
        digests are still used, and with no project `ocx.toml` in scope the command exits 0 with \
        no output.\n\n\
        The `.envrc` line is yours to edit, for example `eval \"$(ocx direnv export -g ci --env \
        FORCE_COLOR=1)\"`.")]
    Export(super::direnv_export::DirenvExport),
}

impl Direnv {
    /// The leaf this invocation runs; bare `ocx direnv` is `init`.
    pub fn leaf(&self) -> super::leaf::Leaf {
        match &self.command {
            Some(DirenvCommand::Export(_)) => super::leaf::Leaf::DirenvExport,
            Some(DirenvCommand::Init(_)) | None => super::leaf::Leaf::DirenvInit,
        }
    }

    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match &self.command {
            Some(DirenvCommand::Init(init)) => init.execute(context).await,
            Some(DirenvCommand::Export(export)) => export.execute(context).await,
            None => super::direnv_init::DirenvInit::default().execute(context).await,
        }
    }
}
