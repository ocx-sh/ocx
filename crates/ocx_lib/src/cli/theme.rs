// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Central, swappable colour theme.
//!
//! Every style used by stdout data rendering lives here — both *entity*
//! colours (digest, tag, visibility, …) and table/tree *chrome* (header,
//! rule, separators, zebra). Nothing styles inline at a call site, so the
//! whole scheme is replaced by selecting a different [`Theme`] without
//! touching renderers or data types.
//!
//! Each named theme is one constructor in its own submodule
//! ([`colorful`], [`mono`]); [`Theme`] holds only the resolved styles and
//! a stable [`Theme::name`] so a future config field can pick one via
//! [`FromStr`]. Paint methods are plain string transforms over
//! [`console::Style`] — no-ops when colour is disabled, so colour-off
//! output stays byte-identical to the unstyled form. [`StyledInk`] lets a
//! domain value compose itself from coloured parts.

use std::str::FromStr;

use crate::cli::Style;
use crate::oci::{Digest, Identifier};
use crate::package::metadata::visibility::Visibility;

mod colorful;
mod mono;

/// Wrap a `console::Style` as a layout-free [`Style`]. Visible to the
/// per-theme submodules (descendants), not outside `theme`.
const fn s(inner: console::Style) -> Style {
    Style::new().style(inner)
}

/// A resolved colour theme: every style, the active colour decision, and a
/// stable name.
///
/// Cheap to construct (a handful of `console::Style` values); built on
/// demand from the resolved stdout colour so the owning `DataInterface`
/// stays `Copy`. Fields are private and uniform — entity colours are
/// reached through paint methods, chrome through accessors — so a call
/// site never depends on the internal layout.
#[derive(Clone, Debug)]
pub struct Theme {
    name: &'static str,
    color: bool,

    // Entity colours.
    digest: Style,
    tag: Style,
    /// Structural punctuation inside a composed value (e.g. the `@` before
    /// a digest).
    punct: Style,
    repeated: Style,
    /// A short informational note next to a value (media type, byte size,
    /// modifier kind, dispatch-command divergence) — de-emphasised so it
    /// reads as an aside, not the value itself.
    note: Style,
    vis_public: Style,
    vis_private: Style,
    vis_interface: Style,
    vis_sealed: Style,

    /// The key in a labelled-value pair (e.g. `Version: 1.2.3`). Plain bold
    /// so it stands out from the value without the column-header connotation
    /// of [`Self::header`] (which is bold *and* underlined for table contexts).
    label: Style,
    /// A parenthetical or secondary value adjacent to the primary value
    /// (e.g. a build timestamp shown next to a version, or a file path shown
    /// next to a name). Plain dim, matching [`Self::note`] in weight but
    /// separate so the two roles can diverge later without a rename.
    aside: Style,

    // Table / tree chrome.
    header: Style,
    /// Tree connectors, table rule, and column separators.
    chrome: Style,
    hint: Style,
    /// Additive zebra accent layered over odd data rows.
    row_accent: Style,
}

impl Theme {
    /// The default theme ([`colorful`]). `color` is the resolved stdout
    /// colour decision; when `false` every paint method returns its input
    /// unchanged.
    pub fn new(color: bool) -> Self {
        colorful::theme(color)
    }

    /// Returns a copy with the colour decision replaced. Lets a theme
    /// parsed by name (colour-agnostic) adopt the stream's setting.
    #[must_use]
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Stable identifier (`"colorful"`, `"mono"`) — the value a config
    /// `theme = "…"` field would carry.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Whether colour is enabled for this theme.
    pub fn color(&self) -> bool {
        self.color
    }

    /// Header cell style (table).
    pub fn header(&self) -> &Style {
        &self.header
    }

    /// Chrome: tree connectors, table rule, column separators.
    pub fn chrome(&self) -> &Style {
        &self.chrome
    }

    /// Hint / informational message style.
    pub fn hint(&self) -> &Style {
        &self.hint
    }

    /// Additive zebra accent for odd table rows.
    pub fn row_accent(&self) -> &Style {
        &self.row_accent
    }

    fn paint(&self, style: &Style, text: impl AsRef<str>) -> String {
        let text = text.as_ref();
        if self.color {
            // `self.color` is the already-resolved decision (mirrors the
            // Printer's stdout colour: honours --color, NO_COLOR, tty).
            // Force styling so the result is deterministic regardless of
            // console's own tty auto-detection.
            (**style).clone().force_styling(true).apply_to(text).to_string()
        } else {
            text.to_string()
        }
    }

    /// Colour a content digest (`sha256:…`).
    pub fn digest(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.digest, text)
    }

    /// Colour a tag / version / platform / variant token.
    pub fn tag(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.tag, text)
    }

    /// Colour structural punctuation (e.g. `@`).
    pub fn punct(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.punct, text)
    }

    /// Colour a "repeated" marker.
    pub fn repeated(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.repeated, text)
    }

    /// Colour a short informational note (media type, byte size, modifier
    /// kind, dispatch-command divergence) — an aside next to a value.
    pub fn note(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.note, text)
    }

    /// Style the key in a labelled-value pair (plain bold). Distinct from
    /// [`Self::header`], which is bold *and* underlined and reserved for
    /// table column headers.
    pub fn label(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.label, text)
    }

    /// Style a parenthetical or secondary value (plain dim). Distinct from
    /// [`Self::note`] semantically — `note` annotates an entity, `aside`
    /// qualifies a value in a labelled-value display — though both currently
    /// render dim. Kept separate so the two roles can diverge without
    /// breaking callers.
    pub fn aside(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.aside, text)
    }

    /// Colour a visibility tag by value (same mapping everywhere).
    pub fn visibility(&self, vis: Visibility, text: impl AsRef<str>) -> String {
        let style = match (vis.private, vis.interface) {
            (true, true) => &self.vis_public,
            (true, false) => &self.vis_private,
            (false, true) => &self.vis_interface,
            (false, false) => &self.vis_sealed,
        };
        self.paint(style, text)
    }

    /// Render a [`StyledInk`] value to a composed, coloured string.
    pub fn of(&self, value: &impl StyledInk) -> String {
        value.ink(self)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(false)
    }
}

/// A name that does not match any known theme.
#[derive(Debug, thiserror::Error)]
#[error("unknown theme: {0}")]
pub struct UnknownTheme(pub String);

impl FromStr for Theme {
    type Err = UnknownTheme;

    /// Resolves a theme by name (colour-agnostic — combine with
    /// [`Theme::with_color`]). `"default"` aliases the default theme.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "colorful" | "default" => Ok(colorful::theme(false)),
            "mono" => Ok(mono::theme(false)),
            other => Err(UnknownTheme(other.to_string())),
        }
    }
}

/// A domain value that composes its own coloured representation from a
/// [`Theme`]. Implemented in the cli layer so domain types stay
/// presentation-free; same crate as the types, so no orphan impedance.
pub trait StyledInk {
    /// Compose `self` into a styled string using `theme`'s palette. With
    /// colour off this must equal the value's plain `Display`.
    fn ink(&self, theme: &Theme) -> String;
}

impl StyledInk for Digest {
    fn ink(&self, theme: &Theme) -> String {
        theme.digest(self.to_string())
    }
}

impl StyledInk for Identifier {
    fn ink(&self, theme: &Theme) -> String {
        let mut out = format!("{}/{}", self.registry(), self.repository());
        if let Some(tag) = self.tag() {
            out.push_str(&theme.tag(format!(":{tag}")));
        }
        if let Some(digest) = self.digest() {
            out.push_str(&theme.punct("@"));
            out.push_str(&digest.ink(theme));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(spec: &str) -> Identifier {
        Identifier::parse_with_default_registry(spec, "ocx.sh").unwrap()
    }

    #[test]
    fn ink_plain_equals_display_for_all_part_combinations() {
        let theme = Theme::new(false);
        let specs = [
            "ocx.sh/cmake".to_string(),
            "ocx.sh/cmake:3.28".to_string(),
            format!("ocx.sh/cmake@sha256:{}", "a".repeat(64)),
            format!("ocx.sh/cmake:3.28@sha256:{}", "b".repeat(64)),
        ];
        for spec in specs {
            let identifier = id(&spec);
            assert_eq!(
                theme.of(&identifier),
                identifier.to_string(),
                "plain ink must match Display"
            );
        }
    }

    #[test]
    fn ink_colored_strips_back_to_display() {
        let theme = Theme::new(true);
        let identifier = id(&format!("ocx.sh/cmake:3.28@sha256:{}", "a".repeat(64)));
        let inked = theme.of(&identifier);
        assert!(inked.contains("\x1b["), "expected ANSI in colored ink");
        assert_eq!(console::strip_ansi_codes(&inked), identifier.to_string());
    }

    #[test]
    fn paint_is_noop_without_color() {
        let theme = Theme::new(false);
        assert_eq!(theme.digest("sha256:ab"), "sha256:ab");
        assert_eq!(theme.visibility(Visibility::PUBLIC, "public"), "public");
        assert_eq!(theme.note("12 bytes"), "12 bytes");
        assert_eq!(theme.label("Version"), "Version");
        assert_eq!(theme.aside("(2026-05-28)"), "(2026-05-28)");
    }

    #[test]
    fn label_is_bold_in_both_themes() {
        // A key in a labelled-value pair is plain bold (SGR 1). Both shipped
        // themes share this attribute.
        for theme in [colorful::theme(true), mono::theme(true)] {
            let out = theme.label("Version");
            assert!(out.contains("\x1b[1m"), "expected bold SGR: {out:?}");
            assert_eq!(console::strip_ansi_codes(&out), "Version");
        }
    }

    #[test]
    fn aside_is_dim_in_both_themes() {
        // A parenthetical or secondary value is plain dim (SGR 2). Both
        // shipped themes share this attribute.
        for theme in [colorful::theme(true), mono::theme(true)] {
            let out = theme.aside("(2026-05-28)");
            assert!(out.contains("\x1b[2m"), "expected dim SGR: {out:?}");
            assert_eq!(console::strip_ansi_codes(&out), "(2026-05-28)");
        }
    }

    #[test]
    fn note_is_dim_in_both_themes() {
        // An informational note (media type, size, modifier kind) is
        // de-emphasised via SGR 2. Both shipped themes share this attribute.
        for theme in [colorful::theme(true), mono::theme(true)] {
            let out = theme.note("x");
            assert!(out.contains("\x1b[2m"), "expected dim SGR: {out:?}");
            assert_eq!(console::strip_ansi_codes(&out), "x");
        }
    }

    #[test]
    fn default_theme_is_colorful() {
        assert_eq!(Theme::default().name(), "colorful");
        assert_eq!(Theme::new(true).name(), "colorful");
    }

    #[test]
    fn from_str_resolves_known_themes_and_rejects_unknown() {
        assert_eq!("colorful".parse::<Theme>().unwrap().name(), "colorful");
        assert_eq!("default".parse::<Theme>().unwrap().name(), "colorful");
        assert_eq!("mono".parse::<Theme>().unwrap().name(), "mono");
        assert!("plaid".parse::<Theme>().is_err());
    }

    #[test]
    fn from_str_is_color_agnostic_until_with_color() {
        let theme = "mono".parse::<Theme>().unwrap();
        assert!(!theme.color());
        assert!(theme.with_color(true).color());
    }

    #[test]
    fn row_accent_is_dim_in_both_themes() {
        // Zebra rows are dimmed (SGR 2); the accent layers additively over
        // each cell's own colour. Both shipped themes share this attribute.
        for theme in [colorful::theme(true), mono::theme(true)] {
            let out = (**theme.row_accent())
                .clone()
                .force_styling(true)
                .apply_to("x")
                .to_string();
            assert!(out.contains("\x1b[2m"), "expected dim SGR: {out:?}");
        }
    }

    #[test]
    fn mono_theme_uses_no_hue() {
        let out = mono::theme(true).digest("sha256:ab");
        assert!(out.contains("\x1b["));
        assert!(!out.contains("38;5;"), "mono must not emit 256-colour codes: {out:?}");
    }
}
