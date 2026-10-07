// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use console::{Style, Term};
use ocx_shell::shell;
use ocx_util::fs::path::AbsolutePath;

use crate::api::Printable;
use crate::app::Context;

#[derive(Parser)]
pub struct About;

/// Isometric cube logo rendered with `+` and `=` characters.
#[rustfmt::skip]
const LOGO: [&str; 21] = [
    "              ++++++               ++++++",
    "          ++++++++++++++       ++++++++++++++",
    "         +++++++++++++++++   +++++++++++++++++",
    "       ++++ ++++++++++  +++++++  ++++++++++ +++=",
    "       +++++++  ++= +++++++++++++++ +++ =++++++=",
    "       ++++++++++  +++++++++++++++++  +++++++++=",
    "       ++++++++++ ++++ +++++++++ =+++ +++++++++=",
    "        +++++++++ +++++++  +  +++++++ +++++++++",
    "           ++++++ +++++++++ +++++++++ ++++++",
    "              +++ +++++++++ +++++++++ +++",
    "                  +++++++++ +++++++++",
    "              ++++++  +++++ +++++  ++++++",
    "           +++++++++++++ ++ ++ +++++++++++++",
    "         +++++++++++++++++   +++++++++++++++++",
    "       ++++ ++++++++++  +++ +++  ++++++++++ +++=",
    "       +++++++  +++ +++++++ +++++++ +++  ++++++=",
    "       ++++++++++ +++++++++ +++++++++ +++++++++=",
    "       ++++++++++ +++++++++ +++++++++ +++++++++=",
    "       ++++++++++ +++++++++ +++++++++ +++++++++=",
    "          +++++++ ++++++       ++++++ ++++++=",
    "              +++ +++              ++ +++",
];

const LOGO_WIDTH: usize = 52;

impl About {
    pub async fn execute(&self, context: Context) -> anyhow::Result<ExitCode> {
        let version = crate::app::version().to_string();
        let registry = context.default_registry().to_string();
        let host_platform = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
        let current_shell = shell::Shell::from_process().map(|s| format!("{s}"));
        let root = context.file_structure().root();
        let absolute = std::path::absolute(root).map_err(|error| ocx_util::error::FileError::new(root, error))?;
        let home = AbsolutePath::new(absolute)
            .ok_or_else(|| ocx_util::error::FileError::new(root, std::io::ErrorKind::InvalidInput.into()))?;
        // The cache `Context::try_init` populated; no second probe.
        let libc: Vec<String> = ocx_oci::cached_libc_labels();

        let info = crate::api::data::about::About::new(version, registry, &host_platform, libc, current_shell, home);

        // The loader's own warning lands on a stderr the shims discard, so this command must report
        // the strip; on stderr, so `--format json`'s stdout stays one document.
        if let Some(reason) = context
            .config()
            .shell
            .as_ref()
            .and_then(|shell| shell.consent_strip_reason.as_deref())
        {
            context.ui().warn(reason);
        }

        let data = context.api().data();
        if context.api().is_json() {
            context.api().report(&info)?;
        // Logo on a terminal (unstyled under `--color never`), or whenever colour is forced.
        } else if Term::stdout().is_term() || data.color() {
            self.print_logo(&info, data.color())?;
        } else {
            info.print_plain(data);
        }

        Ok(ExitCode::SUCCESS)
    }

    fn print_logo(&self, info: &crate::api::data::about::About, color: bool) -> anyhow::Result<()> {
        let term = Term::stdout();

        let logo_style = if color {
            Style::new().color256(203)
        } else {
            Style::new()
        };
        let label_style = if color { Style::new().bold() } else { Style::new() };
        let dim_style = if color { Style::new().dim() } else { Style::new() };

        let platforms = info.plain_platforms.join(", ");
        let libc = info.libc.join(", ");
        let shell_str = info.shell.as_deref().unwrap_or("n/a");
        let home = info.home.as_path().display().to_string();
        let commit_summary = info.commit_summary();

        let mut info_entries: Vec<(&str, &str)> = Vec::with_capacity(7);
        info_entries.push(("Version", &info.version));
        if let Some(commit) = commit_summary.as_deref() {
            info_entries.push(("Commit", commit));
        }
        if let Some(channel) = info.provenance.channel {
            info_entries.push(("Channel", channel));
        }
        info_entries.push(("Registry", &info.registry));
        info_entries.push(("Platform", &platforms));
        if !info.libc.is_empty() {
            info_entries.push(("Libc", &libc));
        }
        info_entries.push(("Shell", shell_str));
        info_entries.push(("Home", &home));

        let info_lines: Vec<String> = info_entries
            .iter()
            .map(|(label, value)| format!("{} {}", label_style.apply_to(format!("{label:<10}")), value))
            .collect();

        let info_offset = (LOGO.len().saturating_sub(info_lines.len())) / 2;
        let gap = "  ";

        term.write_line("")?;

        for (i, logo_line) in LOGO.iter().enumerate() {
            let info_part = i
                .checked_sub(info_offset)
                .and_then(|idx| info_lines.get(idx))
                .map(String::as_str)
                .unwrap_or("");

            let padding = " ".repeat(LOGO_WIDTH.saturating_sub(logo_line.len()));

            term.write_line(&format!(
                "{}{}{gap}{info_part}",
                logo_style.apply_to(logo_line),
                padding,
            ))?;
        }

        let url = "https://ocx.sh";
        let url_padding = (LOGO_WIDTH.saturating_sub(url.len())) / 2;
        term.write_line(&format!("\n{}{}", " ".repeat(url_padding), dim_style.apply_to(url)))?;

        term.write_line("")?;

        Ok(())
    }
}
