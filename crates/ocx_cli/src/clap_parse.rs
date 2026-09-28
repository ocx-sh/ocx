// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Clap parse boundary: every clap failure mapped to an [`ExitCode`].
//!
//! Named `clap_parse`, not `clap`: a crate-root `clap` module makes every `use clap::…` ambiguous.

use clap_builder::error::ErrorKind as ClapErrorKind;
use clap_builder::{ArgMatches, Command};

use ocx_exit::ExitCode;

/// Parse `argv` (program name first). Help and version print and exit the process; any other
/// clap error prints to stderr and returns `Err(ExitCode::UsageError)`, which the caller must not
/// log again — clap's message is the complete diagnostic.
pub fn parse(cmd: Command, argv: &[std::ffi::OsString]) -> Result<ArgMatches, ExitCode> {
    match cmd.try_get_matches_from(argv) {
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
