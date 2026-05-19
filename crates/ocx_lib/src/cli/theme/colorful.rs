// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Expressive 256-colour theme (project default).

use super::{Theme, s};

pub(super) fn theme(color: bool) -> Theme {
    Theme {
        name: "colorful",
        color,
        digest: s(console::Style::new().color256(117)), // light sky blue
        tag: s(console::Style::new().color256(80)),     // cyan-teal
        punct: s(console::Style::new().dim()),
        repeated: s(console::Style::new().italic().dim()),
        note: s(console::Style::new().dim()),
        vis_public: s(console::Style::new().color256(114)), // soft green
        vis_private: s(console::Style::new().italic().color256(179)), // warm amber
        vis_interface: s(console::Style::new().italic().color256(141)), // lavender
        vis_sealed: s(console::Style::new().italic().dim().color256(245)), // muted gray
        header: s(console::Style::new().bold().underlined()),
        chrome: s(console::Style::new().dim()),
        hint: s(console::Style::new().dim().italic().underlined()),
        row_accent: s(console::Style::new().dim()),
    }
}
