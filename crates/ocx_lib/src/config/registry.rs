// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-registry configuration.
//!
//! Home for all `[registries.<name>]` settings. The struct is live in v1
//! with only `url` defined, but is where future per-registry fields land —
//! `insecure`, `location` rewrite, `timeout`, auth — without forcing
//! migration of existing configs.

use serde::Deserialize;

/// Configuration for a single `[registries.<name>]` entry.
///
/// Re-exported from [`crate::config`] as `RegistryConfig`. `deny_unknown_fields`
/// is enforced so typos inside a known section fail fast.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryConfig {
    /// The registry hostname this entry resolves to. When
    /// [`super::RegistryGlobals::default`] names this entry, OCX uses `url`
    /// as the effective default registry hostname for bare identifiers.
    pub url: Option<String>,
}

impl RegistryConfig {
    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    pub fn merge(&mut self, other: RegistryConfig) {
        if other.url.is_some() {
            self.url = other.url;
        }
    }
}
