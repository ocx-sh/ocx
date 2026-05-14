// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::Write as _;
use std::ops::Deref;

/// The single write point for the CLI. Owns the per-stream color decision.
///
/// Callers never call `console::Style::apply_to` themselves and never branch
/// on color. They build a line through the fluent [`Line`] builder returned
/// by [`Printer::cout`] / [`Printer::cerr`], declaring each segment's text
/// and intended [`Style`]; the builder applies color only when the target
/// stream's color is enabled — but layout (alignment / margin) is *always*
/// applied so columns line up identically with and without color.
///
/// ```ignore
/// printer.cerr()
///     .render("warning:", &STYLE_WARN)   // colored iff stderr color on
///     .plain(" disk almost full")          // never colored
///     .end_line();                          // emit with trailing '\n'
/// ```
///
/// `push_style` / `pop_style` layer an extra color (e.g. a background) over
/// every following segment until popped. Because `console::Style` values do
/// not merge, layering is done by nesting (`pushed.apply_to(seg.apply_to(t))`):
/// fine for a backdrop, but two conflicting attributes resolve to the inner.
/// Layout on a pushed style is ignored — only [`Line::render`]'s own `style`
/// argument drives alignment.
#[derive(Clone, Copy, Debug)]
pub struct Printer {
    stdout_color: bool,
    stderr_color: bool,
}

impl Printer {
    pub fn new(stdout_color: bool, stderr_color: bool) -> Self {
        Self {
            stdout_color,
            stderr_color,
        }
    }

    /// Whether stdout color is enabled. Exposed only for the rare caller that
    /// must compute display width before writing (e.g. `info` logo layout),
    /// where ANSI in the measured string would break alignment.
    pub fn stdout_color(&self) -> bool {
        self.stdout_color
    }

    /// Begin a line targeting **stdout**.
    pub fn cout(&self) -> Line {
        Line::new(Target::Stdout, self.stdout_color)
    }

    /// Begin a line targeting **stderr**.
    pub fn cerr(&self) -> Line {
        Line::new(Target::Stderr, self.stderr_color)
    }
}

/// Horizontal alignment used by [`Style::apply`] when a [`Style::margin`] is
/// set and the text is narrower than the margin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Alignment {
    #[default]
    Left,
    Right,
    Center,
}

/// A CLI cell style: a `console::Style` plus layout (alignment + margin).
///
/// `Style` [`Deref`]s to the inner [`console::Style`], so it is a drop-in
/// where a `console::Style` was used for coloring (`style.apply_to(x)`,
/// `style.force_styling(..)`, …). The added [`Style::apply`] pads the text to
/// [`Style::margin`] columns per [`Style::alignment`] — letting tables and
/// trees align by margin instead of `format!("{:width$}")` plus a separate
/// coloring pass, which double-counts ANSI bytes and breaks alignment under
/// color.
///
/// Layout is color-independent: [`Line::render`] always calls [`Style::apply`]
/// and only conditionally applies color, so a column is the same width with
/// `--color never` and `--color always`.
///
/// Built with `const fn` builders so `STYLE_*` items stay `const`:
///
/// ```ignore
/// const HDR: Style = Style::new()
///     .margin_left(12)
///     .style(console::Style::new().underlined());
/// ```
#[derive(Clone, Debug)]
pub struct Style {
    alignment: Alignment,
    margin: usize,
    style: console::Style,
}

impl Style {
    /// An unstyled, zero-margin, left-aligned style. `const` so call sites
    /// can declare `const STYLE_X: Style = Style::new()...;`.
    pub const fn new() -> Self {
        Self {
            alignment: Alignment::Left,
            margin: 0,
            style: console::Style::new(),
        }
    }

    /// Replace the color/attribute layer with `style`.
    pub const fn style(mut self, style: console::Style) -> Self {
        self.style = style;
        self
    }

    /// Set the alignment used when padding to [`Self::margin`].
    pub const fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Minimum column width. [`Self::apply`] pads narrower text to this many
    /// display columns; `0` (default) disables padding entirely.
    pub const fn margin(mut self, margin: usize) -> Self {
        self.margin = margin;
        self
    }

    /// `margin(margin)` + left alignment (pad on the right).
    pub const fn margin_left(mut self, margin: usize) -> Self {
        self.margin = margin;
        self.alignment = Alignment::Left;
        self
    }

    /// `margin(margin)` + right alignment (pad on the left).
    pub const fn margin_right(mut self, margin: usize) -> Self {
        self.margin = margin;
        self.alignment = Alignment::Right;
        self
    }

    /// `margin(margin)` + center alignment (pad both sides, extra space on
    /// the right when the padding is odd).
    pub const fn margin_center(mut self, margin: usize) -> Self {
        self.margin = margin;
        self.alignment = Alignment::Center;
        self
    }

    /// Pad `text` with spaces to [`Self::margin`] display columns per
    /// [`Self::alignment`]. Returns `text` unchanged when it is already at
    /// least `margin` wide (never truncates) or when `margin == 0`.
    ///
    /// This is the layout half of a style; the color half is the inner
    /// `console::Style` reached through [`Deref`]. Width is measured with
    /// `console::measure_text_width`, so already-styled input still aligns.
    pub fn apply(&self, text: &str) -> String {
        let width = console::measure_text_width(text);
        if width >= self.margin {
            return text.to_string();
        }
        let pad = self.margin - width;
        match self.alignment {
            Alignment::Left => format!("{text}{}", " ".repeat(pad)),
            Alignment::Right => format!("{}{text}", " ".repeat(pad)),
            Alignment::Center => {
                let left = pad / 2;
                format!("{}{text}{}", " ".repeat(left), " ".repeat(pad - left))
            }
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for Style {
    type Target = console::Style;

    fn deref(&self) -> &console::Style {
        &self.style
    }
}

#[derive(Clone, Copy)]
enum Target {
    Stdout,
    Stderr,
}

/// Fluent single-line builder. Every chainable method returns `self`; the
/// line is written only by [`Line::end`] (no newline) or [`Line::end_line`].
/// Color is decided once (the originating stream's setting): when off, every
/// `render` / `push_style` is a no-op color-wise — but [`Style`] layout
/// (alignment / margin) is still applied so output stays aligned.
pub struct Line {
    target: Target,
    color: bool,
    buf: String,
    style_stack: Vec<console::Style>,
}

impl Line {
    fn new(target: Target, color: bool) -> Self {
        Self {
            target,
            color,
            buf: String::new(),
            style_stack: Vec::new(),
        }
    }

    /// Apply the active pushed-style stack (outermost first) over `s`. Color
    /// only — pushed styles never re-align.
    fn layer(&self, s: String) -> String {
        if !self.color {
            return s;
        }
        let mut acc = s;
        for style in self.style_stack.iter().rev() {
            acc = style.apply_to(acc).to_string();
        }
        acc
    }

    /// Append `text`: always laid out per `style` ([`Style::apply`]), then
    /// colored with `style` **iff** the target stream's color is enabled,
    /// then any pushed styles layered on top.
    pub fn render(mut self, text: impl std::fmt::Display, style: &Style) -> Self {
        let aligned = style.apply(&text.to_string());
        let painted = if self.color {
            style.apply_to(&aligned).to_string()
        } else {
            aligned
        };
        self.buf.push_str(&self.layer(painted));
        self
    }

    /// Append `text` verbatim — never colored, never padded (still subject to
    /// pushed styles).
    pub fn plain(mut self, text: impl std::fmt::Display) -> Self {
        let s = self.layer(text.to_string());
        self.buf.push_str(&s);
        self
    }

    /// Append a single space, colored by pushed styles but not by `render` styles.
    pub fn space(mut self) -> Self {
        let s = self.layer(" ".to_string());
        self.buf.push_str(&s);
        self
    }

    /// Append `n` spaces, colored by pushed styles but not by `render` styles.
    pub fn spaces(mut self, n: usize) -> Self {
        let s = self.layer(" ".repeat(n));
        self.buf.push_str(&s);
        self
    }

    /// Push an extra color applied to every following segment until
    /// [`Line::pop_style`]. Only the color layer is used; any margin /
    /// alignment on `style` is ignored. No-op when color is disabled.
    pub fn push_style(mut self, style: Style) -> Self {
        self.style_stack.push(style.style);
        self
    }

    /// Remove the most recently pushed style.
    pub fn pop_style(mut self) -> Self {
        self.style_stack.pop();
        self
    }

    fn write(&self, newline: bool) {
        match self.target {
            Target::Stdout => {
                let mut out = std::io::stdout();
                if newline {
                    let _ = writeln!(out, "{}", self.buf);
                } else {
                    let _ = write!(out, "{}", self.buf);
                    let _ = out.flush();
                }
            }
            Target::Stderr => {
                let mut err = std::io::stderr();
                if newline {
                    let _ = writeln!(err, "{}", self.buf);
                } else {
                    let _ = write!(err, "{}", self.buf);
                }
            }
        }
    }

    /// Emit the accumulated line with no trailing newline (flushes stdout).
    pub fn end(self) {
        self.write(false);
    }

    /// Emit the accumulated line followed by `\n`.
    pub fn end_line(self) {
        self.write(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `const` construction must compile — `STYLE_*` items rely on it.
    const STYLE_CONST: Style = Style::new().margin_right(6).style(console::Style::new().bold());

    #[test]
    fn const_style_carries_layout_and_color() {
        assert_eq!(STYLE_CONST.margin, 6);
        assert_eq!(STYLE_CONST.alignment, Alignment::Right);
        // Inner console::Style reachable through Deref (clone — force_styling
        // consumes self and we cannot move out of a Deref target).
        let inner = STYLE_CONST.clone().style.force_styling(true);
        assert!(inner.apply_to("x").to_string().contains("x"));
    }

    #[test]
    fn apply_left_pads_on_the_right() {
        let s = Style::new().margin_left(5);
        assert_eq!(s.apply("ab"), "ab   ");
    }

    #[test]
    fn apply_right_pads_on_the_left() {
        let s = Style::new().margin_right(5);
        assert_eq!(s.apply("ab"), "   ab");
    }

    #[test]
    fn apply_center_splits_padding_extra_on_the_right() {
        let s = Style::new().margin_center(5);
        // pad = 3 → 1 left, 2 right
        assert_eq!(s.apply("ab"), " ab  ");
    }

    #[test]
    fn apply_is_noop_when_text_at_least_margin_wide() {
        let s = Style::new().margin_left(2);
        assert_eq!(s.apply("abcd"), "abcd");
    }

    #[test]
    fn apply_is_noop_when_margin_zero() {
        let s = Style::new().style(console::Style::new().bold());
        assert_eq!(s.apply("abc"), "abc");
    }

    #[test]
    fn apply_measures_display_width_ignoring_ansi() {
        // A pre-colored 2-column string still pads to width 5, not counting
        // the ANSI escape bytes.
        let colored = console::Style::new()
            .force_styling(true)
            .red()
            .apply_to("ab")
            .to_string();
        let s = Style::new().margin_left(5);
        let out = s.apply(&colored);
        assert_eq!(console::measure_text_width(&out), 5);
    }

    #[test]
    fn line_layout_applied_even_without_color() {
        // color = false: no ANSI, but margin padding still present so a
        // NO_COLOR table aligns identically to a colored one.
        let line = Line::new(Target::Stdout, false).render("ab", &Style::new().margin_left(4));
        assert_eq!(line.buf, "ab  ");
    }

    #[test]
    fn line_color_applied_when_enabled() {
        let style = Style::new()
            .margin_left(4)
            .style(console::Style::new().force_styling(true).red());
        let line = Line::new(Target::Stdout, true).render("ab", &style);
        // Padded to 4 display columns and wrapped in ANSI.
        assert_eq!(console::measure_text_width(&line.buf), 4);
        assert!(line.buf.len() > 4, "expected ANSI escapes in {:?}", line.buf);
    }
}
