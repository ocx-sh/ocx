// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-mirror test entry configuration.
//!
//! Each [`TestEntry`] declares one test command that `ocx-mirror push` will
//! require to pass for every `(version, platform, container)` before publishing.

use serde::Deserialize;

/// A single test to run against an installed package.
///
/// The `name` is used as the JUNIT testcase name and must be unique within
/// the containing `mirror.yml`. The `command` is executed verbatim in the
/// configured shell.
#[derive(Debug, Clone, Deserialize)]
pub struct TestEntry {
    /// Unique test name. Must match `^[a-zA-Z][a-zA-Z0-9_-]*$`.
    pub name: String,
    /// Single-line shell command. Multi-line scripts must be authored as
    /// files and invoked here (e.g. `bash ./tests/smoke.sh`).
    pub command: String,
}
