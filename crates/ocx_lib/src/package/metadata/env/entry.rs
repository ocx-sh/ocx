// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Resolved env-var entry produced by [`super::resolver::EnvResolver`].

use super::modifier::ModifierKind;

/// A single resolved env-var binding (key, value, kind) ready for application
/// or programmatic consumption.
pub struct Entry {
    pub key: String,
    pub value: String,
    pub kind: ModifierKind,
}
