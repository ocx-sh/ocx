// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the only reach here sits inside macro arguments.

pub mod project;

pub fn render() -> String {
    format!("{}", self::project::MARKER)
}
