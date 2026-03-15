// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use console::{Style, Term};
use ocx_lib::{env, file_structure, oci, shell};

use crate::api::Reportable;
use crate::app::Context;

#[derive(Parser)]
pub struct Info;

/// Isometric cube logo rendered with `+` and `=` characters.
/// 21 lines tall, max 52 chars wide.
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

impl Info {
    pub async fn execute(&self, context: Context) -> anyhow::Result<ExitCode> {
        let version = env!("CARGO_PKG_VERSION").to_string();
        let registry = env::string("OCX_DEFAULT_REGISTRY", oci::DEFAULT_REGISTRY.into());
        let platforms: Vec<String> = crate::conventions::supported_platforms()
            .iter()
            .map(oci::Platform::to_string)
            .collect();
        let current_shell = shell::Shell::from_process().map(|s| format!("{s}"));
        let home = file_structure::default_ocx_root()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.ocx".to_string());

        let info = crate::api::data::info::Info::new(version, registry, platforms, current_shell, home);

        let printer = context.api().printer();
        if context.api().is_json() {
            context.api().report(&info)?;
        // Show the logo in terminals (even with --color never, just unstyled),
        // and also when color is forced (--color always) even if piped.
        } else if Term::stdout().is_term() || printer.color() {
            self.print_logo(&info, printer.color())?;
        } else {
            info.print_plain(printer);
        }

        Ok(ExitCode::SUCCESS)
    }

    fn print_logo(&self, info: &crate::api::data::info::Info, color: bool) -> anyhow::Result<()> {
        let term = Term::stdout();

        let logo_style = if color {
            Style::new().color256(203)
        } else {
            Style::new()
        };
        let label_style = if color { Style::new().bold() } else { Style::new() };
        let dim_style = if color { Style::new().dim() } else { Style::new() };

        let platforms = info.platforms.join(", ");
        let shell_str = info.shell.as_deref().unwrap_or("n/a");

        let info_entries: &[(&str, &str)] = &[
            ("Version", &info.version),
            ("Registry", &info.registry),
            ("Platform", &platforms),
            ("Shell", shell_str),
            ("Home", &info.home),
        ];

        let info_lines: Vec<String> = info_entries
            .iter()
            .map(|(label, value)| format!("{} {}", label_style.apply_to(format!("{label:<10}")), value))
            .collect();

        // Center info lines vertically alongside the logo
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

        // Center the URL under the logo
        let url = "https://ocx.sh";
        let url_padding = (LOGO_WIDTH.saturating_sub(url.len())) / 2;
        term.write_line(&format!("\n{}{}", " ".repeat(url_padding), dim_style.apply_to(url)))?;

        term.write_line("")?;

        Ok(())
    }
}
