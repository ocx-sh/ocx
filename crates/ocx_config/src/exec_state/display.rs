// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Making a state-derived string safe to print.

/// Removes the strip set from a state-derived string before it reaches a terminal: ASCII controls (U+0000-U+001F),
/// DEL, U+0080-U+009F (including the single-byte CSI U+009B) and the bidi controls U+061C, U+200E,
/// U+200F, U+202A-U+202E and U+2066-U+2069.
pub fn display_safe(text: &str) -> String {
    text.chars().filter(|c| !is_stripped(*c)).collect()
}

fn is_stripped(c: char) -> bool {
    matches!(
        c,
        '\u{0000}'..='\u{001F}'
            | '\u{007F}'
            | '\u{0080}'..='\u{009F}'
            | '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests;
