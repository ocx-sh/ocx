// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io;

use crate::cli::{Printer, Style};

// ── Semantic styles for stderr diagnostics ───────────────────────

const STYLE_STATUS_ACTION: Style = Style::new().style(console::Style::new().green().bold());
const STYLE_STATUS_MESSAGE: Style = Style::new().style(console::Style::new().underlined());
const STYLE_PROMPT_LABEL: Style = Style::new().style(console::Style::new().bold());
const STYLE_WARNING_PREFIX: Style = Style::new().style(console::Style::new().yellow().bold());
const STYLE_SUCCESS: Style = Style::new().style(console::Style::new().green().bold());

/// stderr diagnostics and interactive input interface.
///
/// Owns all human-facing stderr output (status lines, warnings, success
/// messages) and interactive prompts. When quiet or non-interactive, diagnostic
/// output routes to the `log` crate instead so it can be filtered or captured
/// by the subscriber.
#[derive(Clone, Copy)]
pub struct UserInterface {
    printer: Printer,
    interactive: bool,
    quiet: bool,
}

impl UserInterface {
    pub fn new(printer: Printer, interactive: bool, quiet: bool) -> Self {
        Self {
            printer,
            interactive,
            quiet,
        }
    }

    /// Whether stdin/stderr is an interactive TTY. Callers use this to fail
    /// early with an actionable hint (e.g. `--password-stdin`) instead of
    /// attempting an interactive prompt that can only error here.
    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Cargo-style diagnostic line to stderr: `action` styled green+bold, then
    /// `message` plain. No padding — OCX emits a single status line per command,
    /// so a fixed-width pad would render as stray leading indent.
    ///
    /// Non-interactive or quiet: routes to `log::info!`.
    pub fn status(&self, action: &str, message: impl std::fmt::Display) {
        if self.quiet || !self.interactive {
            crate::log::info!("{action}: {message}");
            return;
        }
        self.printer
            .cerr()
            .render(action, &STYLE_STATUS_ACTION)
            .space()
            .render(message, &STYLE_STATUS_MESSAGE)
            .end_line();
    }

    /// Warning line to stderr: yellow-bold `warning:` prefix + plain message.
    ///
    /// Non-interactive or quiet: routes to `log::warn!`.
    pub fn warn(&self, message: impl std::fmt::Display) {
        if self.quiet || !self.interactive {
            crate::log::warn!("{message}");
            return;
        }
        self.printer
            .cerr()
            .render("warning:", &STYLE_WARNING_PREFIX)
            .plain(format!(" {message}"))
            .end_line();
    }

    /// Success line to stderr (green+bold when stderr color enabled).
    ///
    /// stderr — not stdout — because a success message is a human diagnostic,
    /// not machine-parseable data; stdout is the CLI's data interface (JSON /
    /// TSV tables only).
    ///
    /// Non-interactive or quiet: routes to `log::info!`.
    pub fn success(&self, message: impl std::fmt::Display) {
        if self.quiet || !self.interactive {
            crate::log::info!("{message}");
            return;
        }
        self.printer.cerr().render(message, &STYLE_SUCCESS).end_line();
    }

    /// Blank separator line on stderr.
    ///
    /// Non-interactive or quiet: no-op (blank lines are pure visual; the log
    /// subscriber manages its own line discipline).
    pub fn status_break(&self) {
        if self.quiet || !self.interactive {
            return;
        }
        self.printer.cerr().end_line();
    }

    /// Prompt the user for a line of text on stderr, read from stdin.
    ///
    /// Returns `Err(Unsupported)` when non-interactive; returns
    /// `Err(UnexpectedEof)` when stdin yields an empty line.
    pub fn prompt_line(&self, label: &str) -> io::Result<String> {
        if !self.interactive {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "non-interactive: cannot prompt for input",
            ));
        }
        self.printer.cerr().render(label, &STYLE_PROMPT_LABEL).end();
        let mut buf = String::new();
        let read = std::io::stdin().read_line(&mut buf)?;
        if read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty input"));
        }
        let trimmed = buf.trim_end_matches(['\r', '\n']).to_string();
        if trimmed.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty input"));
        }
        Ok(trimmed)
    }

    /// Prompt the user for a secret (password) without echo.
    ///
    /// Returns `Err(Unsupported)` when non-interactive.
    pub fn prompt_secret(&self, label: &str) -> io::Result<String> {
        if !self.interactive {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "non-interactive: cannot prompt for input",
            ));
        }
        self.printer.cerr().render(label, &STYLE_PROMPT_LABEL).end();
        rpassword::prompt_password("").map_err(io::Error::other)
    }
}
