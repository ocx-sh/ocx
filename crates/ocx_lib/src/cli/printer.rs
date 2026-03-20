// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt::Write;

use serde::Serialize;

use crate::Result;

/// Stdout output helper that carries the resolved color setting.
///
/// Used by [`Printable`] implementations to format plain-text tables.
/// When color is enabled, headers are underlined and data rows alternate
/// between normal and reversed styles.
#[derive(Clone, Copy, Debug)]
pub struct Printer {
    color: bool,
}

const GAP: &str = "  ";

impl Printer {
    pub fn new(color: bool) -> Self {
        Self { color }
    }

    /// Whether stdout color is enabled.
    pub fn color(&self) -> bool {
        self.color
    }

    /// Serializes `value` as pretty-printed JSON, with syntax highlighting when color is enabled.
    pub fn print_json(&self, value: &impl Serialize) -> Result<()> {
        if self.color {
            let json_value = serde_json::to_value(value)?;
            println!(
                "{}",
                colored_json::to_colored_json(&json_value, colored_json::ColorMode::On)?
            );
        } else {
            println!("{}", serde_json::to_string_pretty(value)?);
        }
        Ok(())
    }

    /// Prints a table of strings to stdout, with columns aligned based on the longest cell in each column.
    pub fn print_table(&self, headers: &[&str], rows: &[Vec<String>]) {
        let widths = Self::column_widths(headers, rows);
        let max_rows = rows.iter().map(|r| r.len()).max().unwrap_or(0);

        let header_style = console::Style::new().underlined();
        let even_style = console::Style::new();
        let odd_style = console::Style::new().reverse();
        let mut buf = String::new();

        // Header row
        for (i, header) in headers.iter().enumerate() {
            if i > 0 {
                buf.push_str(GAP);
            }
            write!(buf, "{:width$}", header, width = widths[i]).unwrap();
        }
        self.print_styled(&buf, &header_style);
        buf.clear();

        // Data rows
        for i in 0..max_rows {
            for (j, row) in rows.iter().enumerate() {
                if j > 0 {
                    buf.push_str(GAP);
                }
                let cell = row.get(i).map_or("", |c| c.as_str());
                write!(buf, "{:width$}", cell, width = widths[j]).unwrap();
            }
            let style = if i % 2 == 0 { &even_style } else { &odd_style };
            self.print_styled(&buf, style);
            buf.clear();
        }
    }

    fn print_styled(&self, text: &str, style: &console::Style) {
        if self.color {
            println!("{}", style.apply_to(text));
        } else {
            println!("{}", text);
        }
    }

    fn column_widths(headers: &[&str], rows: &[Vec<String>]) -> Vec<usize> {
        let num_cols = headers.len().max(rows.len());
        let mut widths = Vec::with_capacity(num_cols);

        for (i, header) in headers.iter().enumerate() {
            let data_max = rows
                .get(i)
                .map_or(0, |col| col.iter().map(|c| c.len()).max().unwrap_or(0));
            widths.push(header.len().max(data_max));
        }
        // Extra columns beyond headers (shouldn't happen, but be safe)
        for col in rows.iter().skip(headers.len()) {
            widths.push(col.iter().map(|c| c.len()).max().unwrap_or(0));
        }

        widths
    }
}
