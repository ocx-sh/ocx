// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use console::{Style, Term};
use ocx_lib::{file_structure, oci, shell};

use crate::api::Printable;
use crate::app::Context;

#[derive(Parser)]
pub struct About;

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

impl About {
    pub async fn execute(&self, context: Context) -> anyhow::Result<ExitCode> {
        // Effective version (honours dev-deploy `__OCX_BUILD_VERSION`
        // override) — same source the `version` command + lock metadata
        // use, so all three stay aligned.
        let version = crate::app::version().to_string();
        // Reflect the same default registry the rest of the CLI resolves —
        // env var, layered config, then compiled fallback (already merged in
        // Context::default_registry).
        let registry = context.default_registry().to_string();
        // Render the host platform's bare os/arch base (no `+os_features`
        // suffix): `Platform::current()`'s `Display` now carries the detected
        // libc, which the dedicated `Libc` row already shows. `segments()` is
        // the no-features rendering.
        let host_platform = oci::Platform::current().unwrap_or_else(oci::Platform::any);
        let platforms: Vec<String> = vec![host_platform.segments().join("/")];
        let current_shell = shell::Shell::from_process().map(|s| format!("{s}"));
        let home = file_structure::default_ocx_root()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.ocx".to_string());
        // Reuse the libc families the resolution path detected —
        // `Context::try_init` already ran `HostCapabilities::detect_and_cache()`,
        // so this reads the populated cache rather than spawning a second probe.
        // A host may advertise multiple families (e.g. glibc + musl).
        let libc: Vec<String> = oci::cached_libc_labels();

        let info = crate::api::data::about::About::new(version, registry, platforms, libc, current_shell, home);

        let data = context.api().data();
        if context.api().is_json() {
            context.api().report(&info)?;
        // Show the logo in terminals (even with --color never, just unstyled),
        // and also when color is forced (--color always) even if piped.
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

        let platforms = info.platforms.join(", ");
        let libc = info.libc.join(", ");
        let shell_str = info.shell.as_deref().unwrap_or("n/a");
        let commit_summary = info.commit_summary();

        // Build the info-table in a Vec so optional rows (Commit,
        // Channel) only land when their source data was baked into the
        // binary. Local `cargo build` without git → no Commit row;
        // non-dev-deploy build → no Channel row.
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
        info_entries.push(("Home", &info.home));

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
