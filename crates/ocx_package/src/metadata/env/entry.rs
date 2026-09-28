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
    /// The separator a [`ModifierKind::List`] entry folds with; `None` on other
    /// kinds, and on a list entry whose separator
    /// [`reconcile_list_separators`](crate::metadata::env::apply::reconcile_list_separators)
    /// settles before the fold.
    pub separator: Option<String>,
}
