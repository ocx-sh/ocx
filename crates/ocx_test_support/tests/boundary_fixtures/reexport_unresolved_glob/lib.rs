// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture crate root: re-exports `project`'s items by glob, and no
//! `project.rs` / `project/mod.rs` exists to read them from. It sits outside
//! the scanned subtree, so only the re-export table reads it.

mod project;
mod sub;

pub use project::*;
