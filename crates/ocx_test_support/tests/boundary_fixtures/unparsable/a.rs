// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: reaches nothing forbidden.

use crate::shell::hook::Hook;

pub fn render(hook: &Hook) -> String {
    crate::shell::export::render(hook)
}
