// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `::ocx_project::…`, the absolute-path spelling of a reach into an
//! extracted crate, inside macro tokens.

pub fn stale() -> String {
    format!("{}", ::ocx_project::lock::is_stale())
}
