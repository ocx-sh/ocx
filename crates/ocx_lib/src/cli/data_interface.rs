// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::borrow::Cow;

use serde::Serialize;

use crate::Result;
use crate::cli::{Alignment, Printer, Style, Theme};

// All data-rendering styles (entity colours + table/tree chrome) live in
// `cli::Theme`, obtained via `DataInterface::theme()`. Nothing styles here
// inline — swap the theme to restyle every surface.

/// A single annotation on a tree node.
///
/// Each annotation carries text and an optional [`Style`].  When no style is
/// provided, the printer falls back to its default annotation style.
#[derive(Clone)]
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

/// A table column: header text plus an optional default cell [`Style`] and
/// alignment.
///
/// The style is the *fallback* for every cell in the column; an individual
/// [`Cell`] may override it. Alignment governs how cells (and the header) are
/// padded to the column width. Construct from a string for the common
/// unstyled, left-aligned case (`"Digest".into()`) or refine with the
/// builders.
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

    /// Sets the default style applied to every cell in this column (unless a
    /// cell overrides it).
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

/// A single table cell: text plus an optional [`Style`] that overrides the
/// owning [`Column`]'s default.
///
/// Use the `From` conversions for plain cells (`"value".into()`) and
/// [`Cell::with_style`] for per-value colouring (e.g. a visibility tag whose
/// colour depends on the value, mirroring tree [`Annotation`] styling).
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
    /// The primary display text for this node. Receives the active
    /// [`Theme`] so the node can compose a coloured label (e.g.
    /// `theme.of(&identifier)`); the printer emits it verbatim.
    fn label(&self, theme: &Theme) -> String;
    /// Child nodes.
    fn children(&self) -> &[Self]
    where
        Self: Sized;
    /// Annotations appended after the label, separated by `·`. Receives the
    /// active [`Theme`] so the node can pre-ink each annotation; an
    /// annotation with no explicit style is emitted verbatim.
    fn annotations(&self, theme: &Theme) -> Vec<Annotation> {
        let _ = theme;
        Vec::new()
    }
}

/// Stdout structured data interface that carries the resolved stdout color setting.
///
/// Used by [`Printable`] implementations to format plain-text tables, trees,
/// hints, JSON output, and step chains. Table presentation depends on the
/// resolved stdout colour setting — see [`Self::print_table`].
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

    /// The resolved colour theme for stdout. Cheap to build (a handful of
    /// `console::Style` values), so it is created on demand and the
    /// interface stays `Copy`. `Printable` impls call this to colour data
    /// entities (`theme.of(&identifier)`, `theme.visibility(..)`, …).
    pub fn theme(&self) -> Theme {
        Theme::new(self.printer.stdout_color())
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

    /// Prints a table to stdout.
    ///
    /// Two presentations, chosen by the resolved stdout colour setting:
    ///
    /// - **Colour on** (interactive): a decorated table — bold+underlined
    ///   header (no rule line, no `│` separators), columns spaced by [`GAP`],
    ///   per-column / per-cell colouring, and a dim zebra stripe on odd data
    ///   rows.
    /// - **Colour off** (piped, `--color never`, `NO_COLOR`): plain
    ///   space-aligned columns separated by [`GAP`], no glyphs and no rule,
    ///   so machine consumers parsing stdout keep a stable, simple layout.
    ///
    /// `rows` is column-major: `rows[c]` holds the cells of column `c`,
    /// aligned with `columns[c]`. Cell text wider than its header sets the
    /// column width; a [`Cell`] style overrides its [`Column`]'s default.
    pub fn print_table(&self, columns: &[Column], rows: &[Vec<Cell>]) {
        let widths = Self::column_widths(columns, rows);
        let max_rows = rows.iter().map(|r| r.len()).max().unwrap_or(0);

        if self.color() {
            self.print_table_decorated(columns, rows, &widths, max_rows);
        } else {
            self.print_table_plain(columns, rows, &widths, max_rows);
        }
    }

    /// Builds a layout [`Style`] padding to `width` per `alignment`, carrying
    /// `color`'s attributes when present. Layout is applied even with colour
    /// off so both presentations align identically.
    fn cell_style(width: usize, alignment: Alignment, color: Option<&Style>) -> Style {
        let base = Style::new().margin(width).alignment(alignment);
        match color {
            Some(s) => base.style((**s).clone()),
            None => base,
        }
    }

    /// Decorated presentation (colour on) — see [`Self::print_table`].
    ///
    /// Underlined-header variant: no vertical `│` separators and no rule
    /// line — the header is underlined instead; columns are spaced by
    /// [`GAP`]; odd data rows keep the dim zebra for row separation.
    fn print_table_decorated(&self, columns: &[Column], rows: &[Vec<Cell>], widths: &[usize], max_rows: usize) {
        let theme = self.theme();
        let mut header = self.printer.cout();
        for (c, col) in columns.iter().enumerate() {
            if c > 0 {
                // Underline the gap too so the header reads as one
                // continuous line, not per-column segments.
                header = header.render(GAP, theme.header());
            }
            let style = Self::cell_style(widths[c], col.alignment, Some(theme.header()));
            header = header.render(col.header.as_ref(), &style);
        }
        header.end_line();

        for r in 0..max_rows {
            let mut line = self.printer.cout();
            if r % 2 == 1 {
                // Additive: pushed onto the line's style stack so it layers
                // *over* each cell's own colour rather than replacing it.
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

    /// Plain presentation (colour off) — see [`Self::print_table`]. Output is
    /// byte-stable for piped consumers: padded columns joined by [`GAP`].
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

    /// Prints a hint or informational message (dim, italic, underlined).
    pub fn print_hint(&self, text: &str) {
        let theme = self.theme();
        self.printer.cout().render(text, theme.hint()).end_line();
    }

    /// Prints a chain of steps connected by `→` with dim connectors.
    ///
    /// Steps are emitted verbatim (`plain`) so a caller that pre-coloured
    /// them via [`Theme::of`] keeps that styling; the arrows use the
    /// theme's chrome.
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

        // Label is emitted `plain` because it may already be a composed,
        // multi-part coloured string from `theme.of(..)`; wrapping it in an
        // outer style would be cut by the parts' own resets.
        let mut line = self
            .printer
            .cout()
            .render(prefix, theme.chrome())
            .render(connector, theme.chrome())
            .plain(node.label(&theme));
        for ann in &annotations {
            // An annotation with an explicit style is rendered with it;
            // otherwise the text is already theme-inked and is emitted
            // verbatim (re-styling would double-wrap and the parts' resets
            // would cut the outer style).
            line = line.plain(" ").render("·", theme.chrome()).plain(" ");
            line = match &ann.style {
                Some(style) => line.render(ann.text.as_ref(), style),
                None => line.plain(ann.text.as_ref()),
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

    /// Per-column display width: the widest of the header and its cells.
    /// Measured with `console::measure_text_width` so already-styled text
    /// (ANSI escapes) does not inflate the width.
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
