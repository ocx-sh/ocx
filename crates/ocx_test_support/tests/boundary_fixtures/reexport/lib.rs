// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture crate root: `ProjectConfig` is re-exported at the root.

mod project;
mod user;

pub use project::config::ProjectConfig;
