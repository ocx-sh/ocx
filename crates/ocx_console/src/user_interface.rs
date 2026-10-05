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
const STYLE_NOTICE_TITLE: Style = Style::new().style(console::Style::new().bold());
const STYLE_NOTICE_COMMAND: Style = Style::new().style(console::Style::new().cyan());

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

    /// A titled block on stderr: `title` bold, one `  text — run `command`` line per row, then one
    /// `  details: `a`, `b`` line unless `details` is empty, every command highlighted; nothing for no rows,
    /// `log::info!` when quiet or non-interactive.
    pub fn notice(&self, title: &str, rows: &[(&str, &str)], details: &[&str]) {
        if rows.is_empty() {
            return;
        }
        if self.quiet || !self.interactive {
            log::info!("{title}");
            for (text, command) in rows {
                log::info!("{text} — run `{command}`");
            }
            if !details.is_empty() {
                let commands: Vec<String> = details.iter().map(|command| format!("`{command}`")).collect();
                log::info!("details: {}", commands.join(", "));
            }
            return;
        }
        self.printer.cerr().render(title, &STYLE_NOTICE_TITLE).end_line();
        for (text, command) in rows {
            self.printer
                .cerr()
                .plain(format!("  {text} — run "))
                .render(format!("`{command}`"), &STYLE_NOTICE_COMMAND)
                .end_line();
        }
        if let Some((first, rest)) = details.split_first() {
            let mut line = self
                .printer
                .cerr()
                .plain("  details: ")
                .render(format!("`{first}`"), &STYLE_NOTICE_COMMAND);
            for command in rest {
                line = line.plain(", ").render(format!("`{command}`"), &STYLE_NOTICE_COMMAND);
            }
            line.end_line();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The only test in this crate that captures, so no other writer shares the sink.
    #[test]
    fn notice_prints_a_title_one_line_per_row_and_the_details_line() {
        let ui = UserInterface::new(Printer::new(false, false), true, false);
        crate::capture::begin();
        ui.notice(
            "ocx: updates available",
            &[
                ("ocx 0.7.0", "ocx self update"),
                ("project (~/proj): cmake", "ocx update"),
                ("global: shellcheck", "ocx --global update"),
            ],
            &["ocx update --check", "ocx --global update --check"],
        );
        ui.notice("ocx: updates available", &[("ocx 0.7.0", "ocx self update")], &[]);
        ui.notice("ocx: updates available", &[], &["ocx update --check"]);
        let (stdout, stderr) = crate::capture::end();
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            "ocx: updates available\n\
             \x20 ocx 0.7.0 — run `ocx self update`\n\
             \x20 project (~/proj): cmake — run `ocx update`\n\
             \x20 global: shellcheck — run `ocx --global update`\n\
             \x20 details: `ocx update --check`, `ocx --global update --check`\n\
             ocx: updates available\n\
             \x20 ocx 0.7.0 — run `ocx self update`\n"
        );
    }
}
