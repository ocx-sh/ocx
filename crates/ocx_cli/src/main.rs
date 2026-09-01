// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use ocx_lib::log;

use ocx::api;
use ocx::app::{self, classify_error};

#[tokio::main]
async fn main() -> ExitCode {
    match app::run().await {
        Ok(exit_code) => exit_code,
        Err(err) => {
            // {err:#} walks the anyhow source chain so causes (e.g. "permission denied") surface to users.
            // `log::error!` already categorizes the line, so we omit a redundant "Error: " prefix.
            //
            // Neutralized here, at the one boundary every failing command exits
            // through (CWE-150). A cause chain quotes names read off wire
            // documents and filesystem walks, and `tracing-subscriber` passes
            // `\n`, `\r`, NUL and the whole `Cf` bidi set straight to the
            // terminal — see `api::data::sanitize_for_terminal`.
            //
            // This covers every command, including the ones that log nothing of
            // their own, but it does not make the per-command sanitizers
            // redundant: a command that aggregates failures returns only the
            // lowest-index error, so its siblings' chains are printed there and
            // never arrive here. Both sites, measured.
            log::error!("{}", api::data::sanitize_for_terminal(&format!("{err:#}")));
            classify_error(err.as_ref()).into()
        }
    }
}

#[cfg(test)]
mod tests {
    /// The error boundary is the last thing standing between a foreign-authored
    /// name and the operator's terminal, and it is the *only* site that sees
    /// every command's failure. Pinned structurally because asserting the
    /// rendered line would mean driving `main` and capturing stderr, and because
    /// the round-2 defect was not a wrong line but a missing one — two commands
    /// sanitized their own prose while this site re-emitted the same chain raw.
    #[test]
    fn the_error_boundary_is_neutralized() {
        // Single tokens, counted — no whole-call needle. The negative form this
        // replaces (`!contains("log::error!(\"{err:#}\")")`) did not match a raw
        // second boundary log written across three lines, so it passed
        // vacuously; a negative needle that stops matching fails SILENTLY, which
        // is the dangerous direction. A formatter can rewrap a call, but it
        // cannot split `log::error!` or `{err:#}`.
        let code: String = include_str!("main.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            code.matches("log::error!").count(),
            1,
            "one boundary log; a second would be the unsanitized one"
        );
        assert_eq!(
            code.matches("{err:#}").count(),
            1,
            "the cause chain is interpolated exactly once, at that log"
        );
        assert_eq!(
            code.matches("sanitize_for_terminal").count(),
            1,
            "and it is neutralized — an unsanitized boundary log nullifies every print-site \
             sanitizer downstream of it"
        );
    }
}
