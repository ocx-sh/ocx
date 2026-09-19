// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `r#project` and `project` name the same module, so a forbidden
//! entry written plainly has to match the raw spelling too. Every arm reads a
//! segment verbatim, so `r#project` compared against `project` was a reach
//! nothing reported — and the token arm additionally ended the path at the
//! `#`, dropping every segment behind it. One line per arm: the `use` item,
//! the expression path, and the same path inside macro tokens.
//!
//! Zero occurrences in `crates/**`, so the live-tree differential could not
//! have seen this one either.

use crate::r#project::api;

pub fn stale() -> bool {
    crate::r#project::lock::is_stale()
}

pub fn nested() -> bool {
    matches!(crate::r#project::api(), true)
}
