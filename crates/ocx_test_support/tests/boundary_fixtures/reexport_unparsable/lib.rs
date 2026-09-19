// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture crate root: re-exports `ProjectConfig`, and does not parse past
//! the last line. It sits outside the scanned subtree, so only the
//! re-export table reads it.

mod project;
mod sub;

pub use project::config::ProjectConfig;
this is not an item;
