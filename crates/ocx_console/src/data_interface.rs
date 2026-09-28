// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::borrow::Cow;

use serde::Serialize;

use ocx_util::error::SerializationError;

use crate::{Alignment, Printer, Style, Theme};

/// A single annotation on a tree node; without a `style` its text is emitted verbatim.
#[derive(Clone)]
pub struct Annotation {
    pub text: Cow<'static, str>,
    pub style: Option<Style>,
}

impl Annotation {
    /// Creates an unstyled annotation.
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

/// A table column: header text, an optional default cell [`Style`] a [`Cell`] may override, and alignment.
pub struct Column {
    header: Cow<'static, str>,
    style: Option<Style>,
    alignment: Alignment,
}

impl Column {
    /// A left-aligned, unstyled column with the given header.
    pub fn new(header: impl Into<Cow<'static, str>>) -> Self {
        Self {
            header: header.into(),
            style: None,
            alignment: Alignment::Left,
        }
    }

    /// Sets the default style for every cell in this column.
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }

    /// Sets the column alignment (default [`Alignment::Left`]).
    pub fn align(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }
}

impl From<&'static str> for Column {
    fn from(header: &'static str) -> Self {
        Self::new(header)
    }
}

impl From<String> for Column {
    fn from(header: String) -> Self {
        Self::new(header)
    }
}

/// A single table cell: text plus an optional [`Style`] overriding its [`Column`]'s default.
pub struct Cell {
    text: Cow<'static, str>,
    style: Option<Style>,
}

impl Cell {
    /// A cell using its column's default style.
    pub fn new(text: impl Into<Cow<'static, str>>) -> Self {
        Self {
            text: text.into(),
            style: None,
        }
    }

    /// Sets a style for this cell, overriding the column default.
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&'static str> for Cell {
    fn from(text: &'static str) -> Self {
        Self::new(text)
    }
}

/// Trait for types that can be rendered as a tree.
pub trait TreeItem {
    /// The node's label, emitted verbatim, so it may be pre-coloured with the active [`Theme`].
    fn label(&self, theme: &Theme) -> String;
    /// Child nodes.
    fn children(&self) -> &[Self]
    where
        Self: Sized;
    /// Annotations appended after the label, separated by `·`.
    fn annotations(&self, theme: &Theme) -> Vec<Annotation> {
        let _ = theme;
        Vec::new()
    }
}

/// Stdout structured data interface carrying the resolved stdout color setting; `ocx_cli`'s
/// `api::Printable` impls render tables, trees, hints, JSON and step chains through it.
#[derive(Clone, Copy, Debug)]
pub struct DataInterface {
    printer: Printer,
}

const GAP: &str = "  ";

impl DataInterface {
    pub fn new(printer: Printer) -> Self {
        Self { printer }
    }

    /// Whether stdout color is enabled, for callers measuring display width before writing.
    pub fn color(&self) -> bool {
        self.printer.stdout_color()
    }

    /// The stdout colour theme, built on demand so the interface stays `Copy`.
    pub fn theme(&self) -> Theme {
        Theme::new(self.printer.stdout_color())
    }

    /// Serializes `value` as pretty-printed JSON, syntax-highlighted iff stdout color is enabled.
    ///
    /// # Errors
    ///
    /// [`SerializationError`] when the value does not serialize.
    pub fn print_json(&self, value: &impl Serialize) -> Result<(), SerializationError> {
        let rendered = if self.printer.stdout_color() {
            let json_value = serde_json::to_value(value)?;
            colored_json::to_colored_json(&json_value, colored_json::ColorMode::On)?
        } else {
            serde_json::to_string_pretty(value)?
        };
        self.printer.cout().plain(rendered).end_line();
        Ok(())
    }

    /// Prints a table to stdout; `rows` is column-major (`rows[c]` holds column `c`'s cells).
    ///
    /// Colour off (piped, `--color never`, `NO_COLOR`) prints plain padded columns and no glyphs, so machine
    /// consumers keep a stable layout.
    pub fn print_table(&self, columns: &[Column], rows: &[Vec<Cell>]) {
        let widths = Self::column_widths(columns, rows);
        let max_rows = rows.iter().map(|r| r.len()).max().unwrap_or(0);

        if self.color() {
            self.print_table_decorated(columns, rows, &widths, max_rows);
        } else {
            self.print_table_plain(columns, rows, &widths, max_rows);
        }
    }

    /// A layout [`Style`] padding to `width`, carrying `color` when present; applied with colour off too.
    fn cell_style(width: usize, alignment: Alignment, color: Option<&Style>) -> Style {
        let base = Style::new().margin(width).alignment(alignment);
        match color {
            Some(s) => base.style((**s).clone()),
            None => base,
        }
    }

    /// Colour-on presentation: underlined header, no `│` or rule line, a dim zebra on odd rows.
    fn print_table_decorated(&self, columns: &[Column], rows: &[Vec<Cell>], widths: &[usize], max_rows: usize) {
        let theme = self.theme();
        let mut header = self.printer.cout();
        for (c, col) in columns.iter().enumerate() {
            if c > 0 {
                // Underline the gap too, so the header reads as one line.
                header = header.render(GAP, theme.header());
            }
            let style = Self::cell_style(widths[c], col.alignment, Some(theme.header()));
            // `&*`, not `.as_ref()`: `typed-path` (via sigstore) adds a second `AsRef` for `Cow<str>`.
            header = header.render(&*col.header, &style);
        }
        header.end_line();

        for r in 0..max_rows {
            let mut line = self.printer.cout();
            if r % 2 == 1 {
                // Pushed onto the style stack, so it layers over each cell's colour instead of replacing it.
                line = line.push_style(theme.row_accent().clone());
            }
            for (c, col) in columns.iter().enumerate() {
                if c > 0 {
                    line = line.plain(GAP);
                }
                let cell = rows.get(c).and_then(|cells| cells.get(r));
                let text = cell.map_or("", |x| x.text.as_ref());
                let color = cell.and_then(|x| x.style.as_ref()).or(col.style.as_ref());
                let style = Self::cell_style(widths[c], col.alignment, color);
                line = line.render(text, &style);
            }
            line.end_line();
        }
    }

    /// Colour-off presentation: byte-stable padded columns joined by [`GAP`].
    fn print_table_plain(&self, columns: &[Column], rows: &[Vec<Cell>], widths: &[usize], max_rows: usize) {
        let mut buf = String::new();
        for (c, col) in columns.iter().enumerate() {
            if c > 0 {
                buf.push_str(GAP);
            }
            buf.push_str(&Self::cell_style(widths[c], col.alignment, None).apply(col.header.as_ref()));
        }
        self.printer.cout().plain(&buf).end_line();
        buf.clear();

        for r in 0..max_rows {
            for (c, col) in columns.iter().enumerate() {
                if c > 0 {
                    buf.push_str(GAP);
                }
                let text = rows
                    .get(c)
                    .and_then(|cells| cells.get(r))
                    .map_or("", |x| x.text.as_ref());
                buf.push_str(&Self::cell_style(widths[c], col.alignment, None).apply(text));
            }
            self.printer.cout().plain(&buf).end_line();
            buf.clear();
        }
    }

    /// Prints a hint or informational message in the theme's hint style.
    pub fn print_hint(&self, text: &str) {
        let theme = self.theme();
        self.printer.cout().render(text, theme.hint()).end_line();
    }

    /// Prints steps joined by `→`, each emitted verbatim so pre-coloured text keeps its styling.
    pub fn print_steps(&self, steps: &[impl std::fmt::Display]) {
        let theme = self.theme();
        let mut line = self.printer.cout();
        for (i, step) in steps.iter().enumerate() {
            if i > 0 {
                line = line.plain(" ").render("→", theme.chrome()).plain(" ");
            }
            line = line.plain(step.to_string());
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

        let theme = self.theme();
        let annotations = node.annotations(&theme);

        // `plain`: an outer style would be cut by a pre-coloured label's own resets.
        let mut line = self
            .printer
            .cout()
            .render(prefix, theme.chrome())
            .render(connector, theme.chrome())
            .plain(node.label(&theme));
        for ann in &annotations {
            // An unstyled annotation is already theme-inked; re-styling it would be cut by its resets.
            line = line.plain(" ").render("·", theme.chrome()).plain(" ");
            line = match &ann.style {
                Some(style) => line.render(&*ann.text, style),
                None => line.plain(&*ann.text),
            };
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

    /// Per-column display width, the widest of header and cells, not counting ANSI escapes.
    fn column_widths(columns: &[Column], rows: &[Vec<Cell>]) -> Vec<usize> {
        let num_cols = columns.len().max(rows.len());
        let mut widths = Vec::with_capacity(num_cols);

        let cells_max = |cells: &Vec<Cell>| {
            cells
                .iter()
                .map(|c| console::measure_text_width(c.text.as_ref()))
                .max()
                .unwrap_or(0)
        };

        for (c, col) in columns.iter().enumerate() {
            let data_max = rows.get(c).map_or(0, cells_max);
            widths.push(console::measure_text_width(col.header.as_ref()).max(data_max));
        }
        // Extra columns beyond headers (shouldn't happen, but be safe).
        for cells in rows.iter().skip(columns.len()) {
            widths.push(cells_max(cells));
        }

        widths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(headers: &[&str]) -> Vec<Column> {
        headers.iter().map(|h| Column::new(h.to_string())).collect()
    }

    fn col(cells: &[&str]) -> Vec<Cell> {
        cells.iter().map(|c| Cell::new(c.to_string())).collect()
    }

    #[test]
    fn column_widths_matches_header_lengths() {
        let widths = DataInterface::column_widths(&cols(&["Name", "Digest"]), &[]);
        assert_eq!(widths, vec![4, 6]);
    }

    #[test]
    fn column_widths_data_wider_than_header() {
        let widths = DataInterface::column_widths(&cols(&["A"]), &[col(&["Long cell"])]);
        assert_eq!(widths, vec![9]);
    }

    #[test]
    fn column_widths_header_wider_than_data() {
        let widths = DataInterface::column_widths(&cols(&["Header"]), &[col(&["Hi"])]);
        assert_eq!(widths, vec![6]);
    }

    #[test]
    fn column_widths_extra_columns_beyond_headers() {
        let rows = vec![col(&["x"]), col(&["extra", "more"])];
        let widths = DataInterface::column_widths(&cols(&["A"]), &rows);
        assert_eq!(widths, vec![1, 5]);
    }

    #[test]
    fn column_widths_empty_inputs() {
        let widths = DataInterface::column_widths(&[], &[]);
        assert!(widths.is_empty());
    }

    #[test]
    fn column_widths_ignores_ansi_in_cell_text() {
        // A pre-styled 3-column cell must not inflate the width by its escape
        // bytes — measured display width, not byte length.
        let colored = console::Style::new()
            .force_styling(true)
            .red()
            .apply_to("abc")
            .to_string();
        let widths = DataInterface::column_widths(&cols(&["H"]), &[vec![Cell::new(colored)]]);
        assert_eq!(widths, vec![3]);
    }

    #[test]
    fn cell_style_pads_per_alignment_without_color() {
        let left = DataInterface::cell_style(5, Alignment::Left, None);
        assert_eq!(left.apply("ab"), "ab   ");
        let right = DataInterface::cell_style(5, Alignment::Right, None);
        assert_eq!(right.apply("ab"), "   ab");
    }

    #[test]
    fn cell_style_carries_color_attributes() {
        let src = Style::new().style(console::Style::new().force_styling(true).red());
        let style = DataInterface::cell_style(4, Alignment::Left, Some(&src));
        let out = style.apply_to(style.apply("ab")).to_string();
        // Padded to 4 display columns and wrapped in ANSI escapes.
        assert_eq!(console::measure_text_width(&out), 4);
        assert!(out.len() > 4, "expected ANSI escapes in {out:?}");
    }

    #[test]
    fn cell_overrides_column_style_precedence() {
        // The renderer resolves colour as cell.style → column.style → none.
        let col_default = Style::new().style(console::Style::new().red());
        let cell_override = Style::new().style(console::Style::new().green());
        let column = Column::new("C").with_style(col_default);
        let plain_cell = Cell::new("x");
        let styled_cell = Cell::new("y").with_style(cell_override);

        let resolved = |c: &Cell| c.style.as_ref().or(column.style.as_ref()).is_some();
        assert!(resolved(&plain_cell), "plain cell falls back to column style");
        assert!(resolved(&styled_cell), "styled cell keeps its override");
        assert!(styled_cell.style.is_some());
        assert!(plain_cell.style.is_none());
    }
}
