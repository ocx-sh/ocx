// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Stdout output helper that carries the resolved color setting.
///
/// Used by [`Reportable`] implementations to format plain-text tables.
/// When color is enabled, headers are bold and separated from data rows
/// by a dim horizontal rule.
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

    /// Prints a table of strings to stdout, with columns aligned based on the longest cell in each column.
    pub fn print_table(&self, headers: &[&str], rows: &[Vec<String>]) {
        let col_widths = Self::column_widths(headers, rows);
        let max_rows = rows.iter().map(|r| r.len()).max().unwrap_or(0);

        if self.color {
            self.print_colored(headers, rows, &col_widths, max_rows);
        } else {
            self.print_plain(headers, rows, &col_widths, max_rows);
        }
    }

    fn print_colored(&self, headers: &[&str], rows: &[Vec<String>], widths: &[usize], max_rows: usize) {
        let header_style = console::Style::new().bold();
        let dim = console::Style::new().dim();

        // Header row
        for (i, header) in headers.iter().enumerate() {
            if i > 0 {
                print!("{GAP}");
            }
            print!("{:width$}", header_style.apply_to(header), width = widths[i]);
        }
        println!();

        // Separator
        for (i, &w) in widths.iter().enumerate() {
            if i > 0 {
                print!("{GAP}");
            }
            print!("{}", dim.apply_to("─".repeat(w)));
        }
        println!();

        // Data rows (alternating dim for readability)
        for i in 0..max_rows {
            let style = if i % 2 == 1 { Some(&dim) } else { None };
            for (j, row) in rows.iter().enumerate() {
                if j > 0 {
                    print!("{GAP}");
                }
                let cell = row.get(i).map_or("", |c| c.as_str());
                let formatted = format!("{:width$}", cell, width = widths[j]);
                match style {
                    Some(s) => print!("{}", s.apply_to(formatted)),
                    None => print!("{formatted}"),
                }
            }
            println!();
        }
    }

    fn print_plain(&self, headers: &[&str], rows: &[Vec<String>], widths: &[usize], max_rows: usize) {
        for (i, header) in headers.iter().enumerate() {
            if i > 0 {
                print!("{GAP}");
            }
            print!("{:width$}", header, width = widths[i]);
        }
        println!();

        for i in 0..max_rows {
            for (j, row) in rows.iter().enumerate() {
                if j > 0 {
                    print!("{GAP}");
                }
                let cell = row.get(i).map_or("", |c| c.as_str());
                print!("{:width$}", cell, width = widths[j]);
            }
            println!();
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
