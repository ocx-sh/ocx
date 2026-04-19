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
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryConfig {
    /// The registry hostname this entry resolves to (e.g. `"ghcr.io"`,
    /// `"registry.company.example"`). When `[registry] default` names this
    /// entry, OCX uses this value as the effective default registry hostname
    /// for bare package identifiers.
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 3.1.2: RegistryConfig::merge None-preserves ────────────────────
    // Plan: unit test gap — lower has Some(url), higher has None → lower preserved.

    #[test]
    fn registry_config_merge_none_in_higher_does_not_clobber_lower_url() {
        // Lower config has a URL set; higher config has None for the same field.
        // After merge, lower's URL must be preserved (None never wins).
        let mut lower = RegistryConfig {
            url: Some("ghcr.io".to_string()),
        };
        let higher = RegistryConfig { url: None };
        lower.merge(higher);
        assert_eq!(
            lower.url.as_deref(),
            Some("ghcr.io"),
            "None in higher should not clobber lower's Some(url)"
        );
    }

    #[test]
    fn registry_config_merge_some_in_higher_overrides_lower_url() {
        // When higher has Some(url), it wins over lower's value.
        let mut lower = RegistryConfig {
            url: Some("old.example".to_string()),
        };
        let higher = RegistryConfig {
            url: Some("new.example".to_string()),
        };
        lower.merge(higher);
        assert_eq!(
            lower.url.as_deref(),
            Some("new.example"),
            "Some in higher should override lower's url"
        );
    }
}
