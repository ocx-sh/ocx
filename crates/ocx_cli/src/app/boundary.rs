// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use crate::api;
use crate::exit::classify_error;

/// The error boundary every `ocx` invocation exits through: `main`, and the
/// in-process seam that stands in for it in tests. One function, so the two
/// cannot drift apart.
///
/// `{error:#}` walks the anyhow source chain so causes (e.g. "permission
/// denied") surface to users. The level already categorises the line, so there
/// is no redundant "Error: " prefix.
///
/// Neutralised here, at the one boundary every failing command exits through
/// (CWE-150). A cause chain quotes names read off wire documents and filesystem
/// walks, and `tracing-subscriber` passes `\n`, `\r`, NUL and the whole `Cf`
/// bidi set straight to the terminal — see `api::data::sanitize_for_terminal`.
///
/// This covers every command, including the ones that log nothing of their
/// own, but it does not make the per-command sanitizers redundant: a command
/// that aggregates failures returns only the lowest-index error, so its
/// siblings' chains are printed there and never arrive here. Both sites,
/// measured.
///
/// `tracing::error!`, not `log::error!`: in production both reach the same
/// subscriber (the `log` bridge), but the seam installs no bridge — it is a
/// process global — so only a `tracing` event reaches its captured stderr.
pub fn finish(result: anyhow::Result<ExitCode>) -> ExitCode {
    match result {
        Ok(code) => code,
        Err(error) => {
            tracing::error!("{}", api::data::sanitize_for_terminal(&format!("{error:#}")));
            classify_error(error.as_ref()).into()
        }
    }
}

#[cfg(test)]
mod tests {
    /// The non-test, non-comment half of a source file.
    fn code(source: &str) -> String {
        source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The error boundary is the last thing standing between a foreign-authored
    /// name and the operator's terminal, and it is the *only* site that sees
    /// every command's failure. Pinned structurally because asserting the
    /// rendered line would mean driving `main` and capturing stderr, and because
    /// the round-2 defect was not a wrong line but a missing one — two commands
    /// sanitized their own prose while this site re-emitted the same chain raw.
    ///
    /// Single tokens, counted — no whole-call needle. A negative needle that
    /// stops matching (a raw second log written across three lines) fails
    /// SILENTLY, which is the dangerous direction. A formatter can rewrap a
    /// call, but it cannot split `error!` or `:#}`.
    #[test]
    fn the_error_boundary_is_neutralized() {
        let boundary = code(include_str!("boundary.rs"));
        assert_eq!(
            boundary.matches("error!").count(),
            1,
            "one boundary log; a second would be the unsanitized one"
        );
        assert_eq!(
            boundary.matches(":#}").count(),
            1,
            "the cause chain is interpolated exactly once, at that log"
        );
        assert_eq!(
            boundary.matches("sanitize_for_terminal").count(),
            1,
            "and it is neutralized — an unsanitized boundary log nullifies every print-site \
             sanitizer downstream of it"
        );
    }

    /// The seam reports a failure through [`super::finish`] and keeps no
    /// boundary of its own — a second copy is one that drifts. `main.rs` is
    /// pinned the same way by its own test (it is another crate root, outside
    /// this target's sources).
    #[test]
    fn the_seam_has_no_error_boundary_of_its_own() {
        let seam = code(include_str!("seam.rs"));
        assert_eq!(seam.matches("error!").count(), 0, "the seam logs no error line itself");
        assert_eq!(seam.matches(":#}").count(), 0, "nor renders a cause chain");
        assert_eq!(
            seam.matches("classify_error").count(),
            0,
            "nor classifies the exit code itself"
        );
        assert_eq!(
            seam.matches("boundary::finish(").count(),
            1,
            "it reports through the shared boundary"
        );
    }
}
