// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap_builder::builder::styling::{AnsiColor, Effects, Styles};

/// Returns clap styles with colored headings and usage when color is enabled,
/// or plain (no styling) when color is disabled.
pub fn clap_styles(color: bool) -> Styles {
    if color {
        Styles::styled()
            .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
            .usage(AnsiColor::Yellow.on_default() | Effects::BOLD)
            .literal(AnsiColor::Green.on_default() | Effects::BOLD)
            .placeholder(AnsiColor::Cyan.on_default())
            .valid(AnsiColor::Green.on_default())
            .invalid(AnsiColor::Red.on_default())
            .error(AnsiColor::Red.on_default() | Effects::BOLD)
    } else {
        Styles::plain()
    }
}
