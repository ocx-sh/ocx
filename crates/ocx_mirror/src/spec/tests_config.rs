// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-mirror test entry configuration.
//!
//! Each [`TestEntry`] declares one test to run against an installed package.
//! Exactly one of `command`, `script`, or `script_inline` must be set —
//! these are mutually exclusive alternatives.

use std::path::PathBuf;

use serde::Deserialize;

/// The resolved kind of a [`TestEntry`], borrowed from the entry's fields.
///
/// Callers (e.g. the CI renderer) consume this enum rather than inspecting the
/// three `Option` fields directly.
#[derive(Debug, Clone, PartialEq)]
pub enum TestKind<'a> {
    /// A shell command string (e.g. `shfmt --version`).
    Command(&'a str),
    /// Path to a `.star` Starlark script file, relative to the mirror repo root.
    Script(&'a std::path::Path),
    /// Inline Starlark script content.
    ScriptInline(&'a str),
}

/// A single test to run against an installed package.
///
/// The `name` is used as the JUnit testcase name and must be unique within the
/// containing `mirror.yml`.  Exactly one of `command`, `script`, or
/// `script_inline` must be set; validation enforces this invariant.
///
/// # Kinds
///
/// | Field | Meaning |
/// |-------|---------|
/// | `command` | Shell command string executed verbatim in the configured shell |
/// | `script` | Path to a Starlark `.star` file relative to the mirror repo root |
/// | `script_inline` | Starlark source inline in the YAML (use `|` block scalar) |
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestEntry {
    /// Unique test name. Must match `^[a-zA-Z][a-zA-Z0-9_-]*$`.
    pub name: String,
    /// Single-line shell command executed in the configured shell.
    #[serde(default)]
    pub command: Option<String>,
    /// Path to a Starlark script file, relative to the mirror repo root.
    #[serde(default)]
    pub script: Option<PathBuf>,
    /// Inline Starlark script source.
    #[serde(default)]
    pub script_inline: Option<String>,
}

impl TestEntry {
    /// Resolve the test kind by checking which payload field is set.
    ///
    /// Returns `Ok(TestKind)` when exactly one field is `Some`; `Err` with a
    /// static message otherwise.  The caller (validator) produces the full
    /// diagnostic that includes the entry name.
    pub fn kind(&self) -> Result<TestKind<'_>, &'static str> {
        match (&self.command, &self.script, &self.script_inline) {
            (Some(cmd), None, None) => Ok(TestKind::Command(cmd.as_str())),
            (None, Some(path), None) => Ok(TestKind::Script(path.as_path())),
            (None, None, Some(src)) => Ok(TestKind::ScriptInline(src.as_str())),
            (None, None, None) => Err("none set"),
            _ => Err("multiple set"),
        }
    }
}
