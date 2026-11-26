// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rendering of config-file text into a terminal warning.

use std::borrow::Cow;

/// `raw` with every control and invisible format character written as `\u{..}`.
///
/// A registry key or host comes from a file a lower tier or a remote payload controls; raw, an escape
/// sequence in it would be executed by the terminal that shows the warning, or a bidi override would
/// reorder the text around it.
pub(crate) fn escape_for_terminal(raw: &str) -> Cow<'_, str> {
    if !raw.chars().any(needs_escape) {
        return Cow::Borrowed(raw);
    }
    Cow::Owned(
        raw.chars()
            .flat_map(|c| {
                let escaped = needs_escape(c).then(|| c.escape_unicode());
                escaped.into_iter().flatten().chain((!needs_escape(c)).then_some(c))
            })
            .collect(),
    )
}

/// `Cc` plus the `Cf` characters that reorder or hide text: bidi controls, zero-width and the BOM.
fn needs_escape(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_borrowed_and_controls_are_escaped() {
        assert!(matches!(escape_for_terminal("ghcr.io:5000"), Cow::Borrowed(_)));
        assert_eq!(escape_for_terminal("a\u{1b}[2Jb"), "a\\u{1b}[2Jb");
        assert_eq!(escape_for_terminal("a\u{202E}b\u{200B}"), "a\\u{202e}b\\u{200b}");
    }
}
