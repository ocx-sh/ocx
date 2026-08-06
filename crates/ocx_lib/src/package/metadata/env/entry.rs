// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Resolved env-var entry produced by [`super::resolver::EnvResolver`].

use super::modifier::ModifierKind;

/// A single resolved env-var binding (key, value, kind) ready for application
/// or programmatic consumption.
#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub value: String,
    pub kind: ModifierKind,
    /// The separator a [`ModifierKind::List`] entry folds with; `None` on every
    /// other kind, and on a list entry whose surface allowed it to be omitted.
    ///
    /// A plain field with no constructor invariant: `Entry`'s fields are `pub`
    /// with struct-literal production sites all over the tree, so an invariant
    /// here would be unenforceable. Each parse boundary validates instead, and
    /// [`reconcile_list_separators`](crate::env::reconcile_list_separators)
    /// settles what a surviving `None` inherits before the fold runs.
    pub separator: Option<String>,
}
