// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::borrow::Cow;

use serde::Serialize;

use crate::Result;
use crate::cli::{Printer, Style};

// ── Semantic styles ──────────────────────────────────────────────

const STYLE_TABLE_HEADER: Style = Style::new().style(console::Style::new().underlined());
const STYLE_TABLE_ROW_EVEN: Style = Style::new();
const STYLE_TABLE_ROW_ODD: Style = Style::new().style(console::Style::new().reverse());
const STYLE_PRINT_HINT: Style = Style::new().style(console::Style::new().dim().italic().underlined());
const STYLE_TREE_LABEL: Style = Style::new().style(console::Style::new().bold());
const STYLE_TREE_CHROME: Style = Style::new().style(console::Style::new().dim());
const STYLE_TREE_ANNOTATION: Style = Style::new().style(console::Style::new().yellow());

/// A single annotation on a tree node.
///
/// Each annotation carries text and an optional [`Style`].  When no style is
/// provided, the printer falls back to its default annotation style.
pub struct Annotation {
    pub text: Cow<'static, str>,
    pub style: Option<Style>,
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
    pub fn with_style(mut self, style: Style) -> Self {
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

/// Stdout structured data interface that carries the resolved stdout color setting.
///
/// Used by [`Printable`] implementations to format plain-text tables, trees,
/// hints, JSON output, and step chains. When color is enabled, headers are
/// underlined and data rows alternate between normal and reversed styles.
///
/// [`Printable`]: crate::api::Printable
#[derive(Clone, Copy, Debug)]
pub struct DataInterface {
    printer: Printer,
}

const GAP: &str = "  ";

impl DataInterface {
    pub fn new(printer: Printer) -> Self {
        Self { printer }
    }

    /// Whether stdout color is enabled. Delegates to the owning [`Printer`];
    /// exposed only for callers that must measure display width before
    /// writing (ANSI would break alignment) — see `command/info.rs` logo.
    pub fn color(&self) -> bool {
        self.printer.stdout_color()
    }

    /// Serializes `value` as pretty-printed JSON, syntax-highlighted iff
    /// stdout color is enabled (the `Printer` owns that decision; JSON
    /// highlighting is `colored_json`, not `console::Style`, so it is
    /// gated here rather than via `paint_out`).
    pub fn print_json(&self, value: &impl Serialize) -> Result<()> {
        let rendered = if self.printer.stdout_color() {
            let json_value = serde_json::to_value(value)?;
            colored_json::to_colored_json(&json_value, colored_json::ColorMode::On)?
        } else {
            serde_json::to_string_pretty(value)?
        };
        self.printer.cout().plain(rendered).end_line();
        Ok(())
    }

    /// Prints a table of strings to stdout, with columns aligned based on the longest cell in each column.
    pub fn print_table(&self, headers: &[&str], rows: &[Vec<String>]) {
        let widths = Self::column_widths(headers, rows);
        let max_rows = rows.iter().map(|r| r.len()).max().unwrap_or(0);

        // Left-align each cell to its column width via `Style::apply` (one
        // pass, ANSI-aware) rather than `format!("{:width$}")` plus a
        // separate coloring pass — the latter double-counts escape bytes and
        // misaligns once color is on. The row color is then applied to the
        // assembled line so the reverse-video band covers the gaps too.
        let column = |i: usize| Style::new().margin_left(widths[i]);
        let mut buf = String::new();

        // Header row
        for (i, header) in headers.iter().enumerate() {
            if i > 0 {
                buf.push_str(GAP);
            }
            buf.push_str(&column(i).apply(header));
        }
        self.print_styled(&buf, &STYLE_TABLE_HEADER);
        buf.clear();

        // Data rows
        for i in 0..max_rows {
            for (j, row) in rows.iter().enumerate() {
                if j > 0 {
                    buf.push_str(GAP);
                }
                let cell = row.get(i).map_or("", |c| c.as_str());
                buf.push_str(&column(j).apply(cell));
            }
            let style = if i % 2 == 0 {
                &STYLE_TABLE_ROW_EVEN
            } else {
                &STYLE_TABLE_ROW_ODD
            };
            self.print_styled(&buf, style);
            buf.clear();
        }
    }

    /// Prints a hint or informational message (dim, italic, underlined).
    pub fn print_hint(&self, text: &str) {
        self.printer.cout().render(text, &STYLE_PRINT_HINT).end_line();
    }

    /// Prints a chain of steps connected by `→`, with bold steps and dim connectors.
    pub fn print_steps(&self, steps: &[impl std::fmt::Display]) {
        let mut line = self.printer.cout();
        for (i, step) in steps.iter().enumerate() {
            if i > 0 {
                line = line.plain(" ").render("→", &STYLE_TREE_CHROME).plain(" ");
            }
            line = line.render(step.to_string(), &STYLE_TREE_LABEL);
        }
        line.end_line();
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

        let mut line = self
            .printer
            .cout()
            .render(prefix, &STYLE_TREE_CHROME)
            .render(connector, &STYLE_TREE_CHROME)
            .render(node.label(), &STYLE_TREE_LABEL);
        for ann in &annotations {
            let style = ann.style.as_ref().unwrap_or(&STYLE_TREE_ANNOTATION);
            line = line
                .plain(" ")
                .render("·", &STYLE_TREE_CHROME)
                .plain(" ")
                .render(ann.text.as_ref(), style);
        }
        line.end_line();

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

    fn print_styled(&self, text: &str, style: &Style) {
        self.printer.cout().render(text, style).end_line();
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
        let widths = DataInterface::column_widths(headers, rows);
        assert_eq!(widths, vec![4, 6]);
    }

    #[test]
    fn column_widths_data_wider_than_header() {
        let headers = &["A"];
        let rows = vec![vec!["Long cell".to_string()]];
        let widths = DataInterface::column_widths(headers, &rows);
        assert_eq!(widths, vec![9]);
    }

    #[test]
    fn column_widths_header_wider_than_data() {
        let headers = &["Header"];
        let rows = vec![vec!["Hi".to_string()]];
        let widths = DataInterface::column_widths(headers, &rows);
        assert_eq!(widths, vec![6]);
    }

    #[test]
    fn column_widths_extra_columns_beyond_headers() {
        let headers = &["A"];
        let rows = vec![vec!["x".to_string()], vec!["extra".to_string(), "more".to_string()]];
        let widths = DataInterface::column_widths(headers, &rows);
        assert_eq!(widths.len(), 2);
        assert_eq!(widths[0], 1);
        assert_eq!(widths[1], 5);
    }

    #[test]
    fn column_widths_empty_inputs() {
        let headers: &[&str] = &[];
        let rows: &[Vec<String>] = &[];
        let widths = DataInterface::column_widths(headers, rows);
        assert!(widths.is_empty());
    }
}
