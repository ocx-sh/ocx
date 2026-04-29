// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{accumulator::DependencyContext, modifier::ModifierKind, var::Var};
use crate::package::metadata::{dependency::DependencyName, template::TemplateResolver};

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
/// Unlike [`Accumulator`](super::accumulator::Accumulator), which writes resolved values
/// into a runtime [`crate::env::Env`], `Exporter` returns the entries as structured data
/// so callers can decide how to apply them. Because `Exporter` only needs the read-only
/// resolution path, it goes directly to [`TemplateResolver`] and skips the dummy
/// [`crate::env::Env`] + per-call `PathBuf` clone that wrapping `Accumulator` previously
/// required (one allocation per env var across every `ocx exec` / `ocx env` invocation).
pub struct Exporter<'a> {
    content: PathBuf,
    dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
    entries: Vec<Entry>,
}

impl<'a> Exporter<'a> {
    pub fn new(content: impl AsRef<Path>, dep_contexts: &'a HashMap<DependencyName, DependencyContext>) -> Self {
        Self {
            content: content.as_ref().to_path_buf(),
            dep_contexts,
            entries: Vec::new(),
        }
    }

    /// Resolves `var` against this exporter's content path + dep contexts and
    /// appends a single [`Entry`] when the var produces a value.
    ///
    /// Calls [`TemplateResolver`] directly so the per-var hot path no longer
    /// allocates a dummy [`crate::env::Env`] or clones the content `PathBuf`
    /// (the previous wrapping path through `Accumulator::resolve_var`).
    pub fn add(&mut self, var: &Var) -> crate::Result<()> {
        if let Some(value) = resolve_var(&self.content, self.dep_contexts, var)? {
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

/// Read-only template resolution for one [`Var`]. Mirrors the body of
/// [`super::accumulator::Accumulator::resolve_var`], minus the [`crate::env::Env`]
/// borrow that the mutating `add` path needs. Splitting the read-only path lets
/// `Exporter` pass `&Path` straight through instead of materialising a fresh
/// `Accumulator` (and its `PathBuf` clone) per var.
fn resolve_var(
    install_path: &Path,
    dep_contexts: &HashMap<DependencyName, DependencyContext>,
    var: &Var,
) -> crate::Result<Option<String>> {
    use crate::package::metadata::env::var::Modifier;

    let template = match var.value() {
        Some(v) => v,
        None => return Ok(None),
    };

    let resolver = TemplateResolver::new(install_path, dep_contexts);
    let mut value = resolver
        .resolve(template)
        .map_err(|source| crate::package::error::Error::EnvVarInterpolation {
            var_key: var.key.clone(),
            source,
        })?;

    if let Modifier::Path(path_modifier) = &var.modifier {
        let mut path = PathBuf::from(&value);
        if path.is_relative() {
            path = install_path.join(path);
        }
        // Sync `path.exists()` is intentional: the entire env-resolution chain
        // is synchronous and called many times per command invocation. A single
        // `stat(2)` against an already-installed package's local content tree
        // is a fast filesystem probe; wrapping in `block_in_place` per call
        // would add scheduler overhead that dominates the probe itself. Switch
        // to `tokio::fs::try_exists` only if the chain ever becomes async
        // end-to-end. (Mirrors the rationale in `Accumulator::resolve_var`.)
        if path_modifier.required && !path.exists() {
            return Err(crate::package::error::Error::RequiredPathMissing(path).into());
        }
        value = path.to_string_lossy().to_string();
    }

    Ok(Some(value))
}
