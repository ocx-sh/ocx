// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Central, swappable colour theme; each named theme is one constructor in a submodule.

use std::str::FromStr;

use crate::Style;

mod colorful;
mod mono;

/// Which of the palette's four visibility colours a caller wants; mapping the domain onto it is the caller's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibilityStyle {
    Public,
    Private,
    Interface,
    Sealed,
}

/// Wrap a `console::Style` as a layout-free [`Style`].
const fn s(inner: console::Style) -> Style {
    Style::new().style(inner)
}

/// A resolved colour theme: every style, the active colour decision, and a stable name.
#[derive(Clone, Debug)]
pub struct Theme {
    name: &'static str,
    color: bool,

    // Entity colours.
    digest: Style,
    tag: Style,
    /// Structural punctuation inside a composed value (e.g. the `@` before a digest).
    punct: Style,
    repeated: Style,
    /// A de-emphasised note next to a value (media type, byte size, modifier kind).
    note: Style,
    vis_public: Style,
    vis_private: Style,
    vis_interface: Style,
    vis_sealed: Style,

    /// The key in a labelled-value pair (e.g. `Version: 1.2.3`).
    label: Style,
    /// A secondary value beside the primary one (e.g. a build timestamp next to a version).
    aside: Style,
    /// A verdict the reader has to act on: a refusal, a lost datum.
    alert: Style,
    /// [`Self::alert`]'s healthy counterpart, a verdict that needs no action.
    ok: Style,

    // Table / tree chrome.
    header: Style,
    /// Tree connectors, table rule, and column separators.
    chrome: Style,
    hint: Style,
    /// Additive zebra accent layered over odd data rows.
    row_accent: Style,
}

impl Theme {
    /// The default theme (`colorful`); with `color` false every paint method returns its input unchanged.
    pub fn new(color: bool) -> Self {
        colorful::theme(color)
    }

    /// Returns a copy with the colour decision replaced, for a theme parsed by name.
    #[must_use]
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Stable identifier (`"colorful"`, `"mono"`).
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
            // Forced, or console's own tty auto-detection overrides the already-resolved decision.
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

    /// Colour a short informational note next to a value.
    pub fn note(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.note, text)
    }

    /// Style the key in a labelled-value pair; table column headers use [`Self::header`].
    pub fn label(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.label, text)
    }

    /// Style a parenthetical or secondary value; [`Self::note`] annotates an entity instead.
    pub fn aside(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.aside, text)
    }

    /// One indented `key: value` line with the key dimmed; an empty `value` renders the key alone, no trailing space.
    pub fn field(&self, indent: &str, key: &str, value: impl AsRef<str>) -> String {
        let value = value.as_ref();
        let key = self.aside(format!("{key}:"));
        if value.is_empty() {
            format!("{indent}{key}")
        } else {
            format!("{indent}{key} {value}")
        }
    }

    /// Style a verdict that needs acting on (an inert shell, a lost prior); never for chrome.
    pub fn alert(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.alert, text)
    }

    /// Style a verdict that needs no action - [`Self::alert`]'s counterpart.
    pub fn ok(&self, text: impl AsRef<str>) -> String {
        self.paint(&self.ok, text)
    }

    /// Colour a visibility tag (same palette entry everywhere).
    pub fn visibility(&self, style: VisibilityStyle, text: impl AsRef<str>) -> String {
        let style = match style {
            VisibilityStyle::Public => &self.vis_public,
            VisibilityStyle::Private => &self.vis_private,
            VisibilityStyle::Interface => &self.vis_interface,
            VisibilityStyle::Sealed => &self.vis_sealed,
        };
        self.paint(style, text)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_is_noop_without_color() {
        let theme = Theme::new(false);
        assert_eq!(theme.digest("sha256:ab"), "sha256:ab");
        assert_eq!(theme.visibility(VisibilityStyle::Public, "public"), "public");
        assert_eq!(theme.note("12 bytes"), "12 bytes");
        assert_eq!(theme.label("Version"), "Version");
        assert_eq!(theme.aside("(2026-05-28)"), "(2026-05-28)");
        assert_eq!(theme.alert("no"), "no");
        assert_eq!(theme.ok("yes"), "yes");
    }

    /// A verdict must be visually separable from its healthy counterpart in
    /// every shipped theme, or "highlighting carries meaning" is a claim the
    /// palette does not back. Both must also stay pure text under colour-off,
    /// which `paint_is_noop_without_color` above pins.
    #[test]
    fn alert_and_ok_are_distinguishable_in_both_themes() {
        for theme in [colorful::theme(true), mono::theme(true)] {
            let alert = theme.alert("no");
            let ok = theme.ok("yes");
            assert!(
                alert.contains("\x1b["),
                "{}: expected SGR on alert: {alert:?}",
                theme.name()
            );
            assert!(ok.contains("\x1b["), "{}: expected SGR on ok: {ok:?}", theme.name());
            assert_ne!(
                console::strip_ansi_codes(&theme.alert("x")),
                "",
                "alert must keep its text"
            );
            assert_ne!(
                theme.alert("x"),
                theme.ok("x"),
                "{}: alert and ok must not render identically",
                theme.name()
            );
        }
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
