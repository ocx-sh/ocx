// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::display_safe;

#[test]
fn strips_c0_del_and_c1_including_csi() {
    assert_eq!(display_safe("a\u{0}b\nc\rd\u{1b}[31me\u{7f}f"), "abcd[31mef");
    assert_eq!(display_safe("x\u{80}y\u{9b}z\u{9f}"), "xyz");
}

#[test]
fn strips_bidi_controls() {
    for c in [
        '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2069}',
    ] {
        assert_eq!(display_safe(&format!("a{c}b")), "ab", "U+{:04X} survived", c as u32);
    }
}

#[test]
fn keeps_printable_text_and_neighbours_of_the_strip_ranges() {
    for text in [
        "/home/user/project",
        "dir with spaces",
        "caf\u{e9}",
        "\u{a0}\u{2029}\u{206a}\u{a1}",
    ] {
        assert_eq!(display_safe(text), text);
    }
}
