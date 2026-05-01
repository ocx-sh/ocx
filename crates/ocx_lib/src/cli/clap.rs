// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Clap parse boundary — single helper that drives `try_get_matches` and maps
//! every clap failure to an [`ExitCode`] so binaries can keep their typed
//! exit-code ladder consistent with the rest of the CLI surface.

use clap_builder::error::ErrorKind as ClapErrorKind;
use clap_builder::{ArgMatches, Command};

use crate::cli::ExitCode;

/// Drive [`Command::try_get_matches`] and classify any failure into an
/// [`ExitCode`].
///
/// - Help / version / `DisplayHelpOnMissingArgumentOrSubcommand` paths
///   delegate to clap's renderer via [`clap_builder::Error::exit`], which
///   prints to stdout and terminates the process with code `0`. Those
///   branches diverge — the function does not return.
/// - Every other clap error (unknown flag, value-validation failure from a
///   field's `FromStr`, missing required argument) is printed to stderr
///   preserving clap's color and suggestion formatting, then surfaced as
///   `Err(ExitCode::UsageError)` (`64`). The caller's top-level handler
///   should return this code without logging an additional generic error
///   line — clap's message is the complete user-facing diagnostic.
///
/// Adopting this helper lets command structs use typed positional fields
/// (e.g. `Vec<Identifier>`) backed by `FromStr` instead of receiving
/// `Vec<String>` and re-parsing in the body. Validation failures still reach
/// users with the `EX_USAGE` (64) code expected by sysexits-aligned tooling.
pub fn parse(cmd: Command) -> Result<ArgMatches, ExitCode> {
    match cmd.try_get_matches() {
        Ok(matches) => Ok(matches),
        Err(err) => {
            if matches!(
                err.kind(),
                ClapErrorKind::DisplayHelp
                    | ClapErrorKind::DisplayVersion
                    | ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                err.exit();
            }
            let _ = err.print();
            Err(ExitCode::UsageError)
        }
    }
}
