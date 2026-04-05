// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::borrow::Cow;
use std::fmt::Write;

use serde::Serialize;

use crate::Result;

// ── Semantic styles ──────────────────────────────────────────────

const STYLE_TABLE_HEADER: console::Style = console::Style::new().underlined();
const STYLE_TABLE_ROW_EVEN: console::Style = console::Style::new();
const STYLE_TABLE_ROW_ODD: console::Style = console::Style::new().reverse();
const STYLE_PRINT_HINT: console::Style = console::Style::new().dim().italic().underlined();
const STYLE_TREE_LABEL: console::Style = console::Style::new().bold();
const STYLE_TREE_CHROME: console::Style = console::Style::new().dim();
const STYLE_TREE_ANNOTATION: console::Style = console::Style::new().yellow();

/// A single annotation on a tree node.
///
/// Each annotation carries text, an optional title, and an optional
/// [`console::Style`].  When no style is provided, the printer falls
/// back to its default annotation style.
pub struct Annotation {
    pub text: Cow<'static, str>,
    pub style: Option<console::Style>,
}

impl Annotation {
    /// Creates an annotation with the printer's default style.
    pub fn new(text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            text: text.into(),
            style: None,
        }
    }

    /// Sets a custom style for this annotation.
    pub fn with_style(mut self, style: console::Style) -> Self {
        self.style = Some(style);
        self
    }
}

/// Trait for types that can be rendered as a tree.
pub trait TreeItem {
    /// The primary display text for this node (shown bold when color is enabled).
    fn label(&self) -> String;
    /// Child nodes.
    fn children(&self) -> &[Self]
    where
        Self: Sized;
    /// Annotations appended after the label, separated by `·`.
    ///
    /// Each annotation carries its own style hint. The printer applies the
    /// annotation's style when present, falling back to a default otherwise.
    fn annotations(&self) -> Vec<Annotation> {
        Vec::new()
    }
}

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

        let header_style = STYLE_TABLE_HEADER;
        let even_style = STYLE_TABLE_ROW_EVEN;
        let odd_style = STYLE_TABLE_ROW_ODD;
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

    /// Prints a hint or informational message (dim, italic, and underlined when color is enabled).
    pub fn print_hint(&self, text: &str) {
        if self.color {
            println!("{}", STYLE_PRINT_HINT.apply_to(text));
        } else {
            println!("{text}");
        }
    }

    /// Prints a chain of steps connected by `→`, with bold steps and dim connectors.
    pub fn print_steps(&self, steps: &[impl std::fmt::Display]) {
        if self.color {
            let parts: Vec<_> = steps
                .iter()
                .map(|s| format!("{}", STYLE_TREE_LABEL.apply_to(s)))
                .collect();
            println!("{}", parts.join(&format!(" {} ", STYLE_TREE_CHROME.apply_to("→"))));
        } else {
            let parts: Vec<_> = steps.iter().map(|s| s.to_string()).collect();
            println!("{}", parts.join(" → "));
        }
    }

    /// Prints a tree rooted at `root` using standard POSIX tree connectors.
    pub fn print_tree<T: TreeItem>(&self, root: &T) {
        self.print_tree_node(root, "", true, true);
    }

    fn print_tree_node<T: TreeItem>(&self, node: &T, prefix: &str, is_last: bool, is_root: bool) {
        let connector = if is_root {
            ""
        } else if is_last {
            "└── "
        } else {
            "├── "
        };

        let annotations = node.annotations();

        if self.color {
            let mut suffix = String::new();
            for ann in &annotations {
                let style = ann.style.as_ref().unwrap_or(&STYLE_TREE_ANNOTATION);
                write!(
                    suffix,
                    " {} {}",
                    STYLE_TREE_CHROME.apply_to("·"),
                    style.apply_to(&ann.text)
                )
                .unwrap();
            }

            println!(
                "{}{}{}{suffix}",
                STYLE_TREE_CHROME.apply_to(prefix),
                STYLE_TREE_CHROME.apply_to(connector),
                STYLE_TREE_LABEL.apply_to(node.label()),
            );
        } else {
            let mut suffix = String::new();
            for ann in &annotations {
                write!(suffix, " · {}", ann.text).unwrap();
            }
            println!("{prefix}{connector}{}{suffix}", node.label());
        }

        let children = node.children();
        let child_prefix = if is_root {
            String::new()
        } else if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };

        for (i, child) in children.iter().enumerate() {
            let child_is_last = i == children.len() - 1;
            self.print_tree_node(child, &child_prefix, child_is_last, false);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_widths_matches_header_lengths() {
        let headers = &["Name", "Digest"];
        let rows: &[Vec<String>] = &[];
        let widths = Printer::column_widths(headers, rows);
        assert_eq!(widths, vec![4, 6]);
    }

    #[test]
    fn column_widths_data_wider_than_header() {
        let headers = &["A"];
        let rows = vec![vec!["Long cell".to_string()]];
        let widths = Printer::column_widths(headers, &rows);
        assert_eq!(widths, vec![9]);
    }

    #[test]
    fn column_widths_header_wider_than_data() {
        let headers = &["Header"];
        let rows = vec![vec!["Hi".to_string()]];
        let widths = Printer::column_widths(headers, &rows);
        assert_eq!(widths, vec![6]);
    }

    #[test]
    fn column_widths_extra_columns_beyond_headers() {
        let headers = &["A"];
        let rows = vec![vec!["x".to_string()], vec!["extra".to_string(), "more".to_string()]];
        let widths = Printer::column_widths(headers, &rows);
        assert_eq!(widths.len(), 2);
        assert_eq!(widths[0], 1);
        assert_eq!(widths[1], 5);
    }

    #[test]
    fn column_widths_empty_inputs() {
        let headers: &[&str] = &[];
        let rows: &[Vec<String>] = &[];
        let widths = Printer::column_widths(headers, rows);
        assert!(widths.is_empty());
    }
}
