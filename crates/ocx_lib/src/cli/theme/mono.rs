// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Restrained monochrome theme — emphasis via weight/effects only, no
//! hue. Reads on any palette; selected by name once theming is wired to
//! configuration.

use super::{Theme, s};

pub(super) fn theme(color: bool) -> Theme {
    Theme {
        name: "mono",
        color,
        digest: s(console::Style::new().bold()),
        tag: s(console::Style::new().underlined()),
        punct: s(console::Style::new().dim()),
        repeated: s(console::Style::new().italic().dim()),
        note: s(console::Style::new().dim()),
        vis_public: s(console::Style::new().bold()),
        vis_private: s(console::Style::new().italic()),
        vis_interface: s(console::Style::new().italic().underlined()),
        vis_sealed: s(console::Style::new().dim()),
        header: s(console::Style::new().bold().underlined()),
        chrome: s(console::Style::new().dim()),
        hint: s(console::Style::new().dim().italic()),
        row_accent: s(console::Style::new().dim()),
    }
}
