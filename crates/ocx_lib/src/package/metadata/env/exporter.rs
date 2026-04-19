// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use super::{
    accumulator::{Accumulator, DependencyContext},
    modifier::ModifierKind,
    var::Var,
};

/// A resolved environment variable entry produced by [`Exporter`].
pub struct Entry {
    pub key: String,
    pub value: String,
    pub kind: ModifierKind,
}

/// Resolves package metadata environment variables into structured entries for programmatic
/// consumption by external tools (Python scripts, Bazel rules, CI steps, etc.).
///
/// Mirrors the API of `shell::ProfileBuilder`: construct with an install content path,
/// call [`add`](Exporter::add) for each metadata var, then [`take`](Exporter::take) to
/// consume the collected entries.
///
/// Unlike [`Accumulator`], which writes resolved values into a runtime [`crate::env::Env`],
/// `Exporter` returns the entries as structured data so callers can decide how to apply them.
pub struct Exporter {
    content: std::path::PathBuf,
    dep_contexts: HashMap<String, DependencyContext>,
    entries: Vec<Entry>,
}

impl Exporter {
    pub fn new(content: impl AsRef<std::path::Path>, dep_contexts: HashMap<String, DependencyContext>) -> Self {
        Self {
            content: content.as_ref().to_path_buf(),
            dep_contexts,
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, var: &Var) -> crate::Result<()> {
        let mut dummy = crate::env::Env::clean();
        let acc = Accumulator::new(&self.content, &self.dep_contexts, &mut dummy);
        if let Some(value) = acc.resolve_var(var)? {
            self.entries.push(Entry {
                key: var.key.clone(),
                value,
                kind: ModifierKind::from(&var.modifier),
            });
        }
        Ok(())
    }

    pub fn take(self) -> Vec<Entry> {
        self.entries
    }
}
