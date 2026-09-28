// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io;

use crate::{Printer, Style};

// ── Semantic styles for stderr diagnostics ───────────────────────

const STYLE_STATUS_ACTION: Style = Style::new().style(console::Style::new().green().bold());
const STYLE_STATUS_MESSAGE: Style = Style::new().style(console::Style::new().underlined());
const STYLE_PROMPT_LABEL: Style = Style::new().style(console::Style::new().bold());
const STYLE_WARNING_PREFIX: Style = Style::new().style(console::Style::new().yellow().bold());
const STYLE_SUCCESS: Style = Style::new().style(console::Style::new().green().bold());

/// stderr diagnostics and interactive prompts; when quiet or non-interactive, diagnostics go to the `log` crate.
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

    /// Whether stdin/stderr is an interactive TTY, so callers can fail early with a hint instead of prompting.
    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Cargo-style status line to stderr, `action` green+bold then `message` underlined, unpadded;
    /// `log::info!` when quiet or non-interactive.
    pub fn status(&self, action: &str, message: impl std::fmt::Display) {
        if self.quiet || !self.interactive {
            log::info!("{action}: {message}");
            return;
        }
        self.printer
            .cerr()
            .render(action, &STYLE_STATUS_ACTION)
            .space()
            .render(message, &STYLE_STATUS_MESSAGE)
            .end_line();
    }

    /// Warning line to stderr with a yellow-bold `warning:` prefix; `log::warn!` when quiet or non-interactive.
    pub fn warn(&self, message: impl std::fmt::Display) {
        if self.quiet || !self.interactive {
            log::warn!("{message}");
            return;
        }
        self.printer
            .cerr()
            .render("warning:", &STYLE_WARNING_PREFIX)
            .plain(format!(" {message}"))
            .end_line();
    }

    /// Success line to stderr, never stdout, which carries data only; `log::info!` when quiet or non-interactive.
    pub fn success(&self, message: impl std::fmt::Display) {
        if self.quiet || !self.interactive {
            log::info!("{message}");
            return;
        }
        self.printer.cerr().render(message, &STYLE_SUCCESS).end_line();
    }

    /// Blank separator line on stderr; a no-op when quiet or non-interactive.
    pub fn status_break(&self) {
        if self.quiet || !self.interactive {
            return;
        }
        self.printer.cerr().end_line();
    }

    /// Prompt on stderr for a line of text read from stdin.
    ///
    /// Errors with `Unsupported` when non-interactive and `UnexpectedEof` on an empty line.
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
