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

    /// Runtime provenance marker: this entry was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`), so it is NON-OVERRIDABLE by any lower
    /// tier for this registry name. Mirrors [`MirrorConfig`](crate::config::MirrorConfig)'s
    /// lock, but per-entry in the `[registries.<name>]` table — a corporate
    /// `[registry] default` naming this entry could otherwise be redirected by
    /// a lower tier overriding this entry's `url` (`Config::resolved_default_registry`
    /// resolves through this table, so a `[registry]` lock alone does not
    /// close the indirection).
    ///
    /// Never serialized — set by the loader via [`Self::lock_as_system`]
    /// after parsing the system-scope file, not read from disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

impl RegistryConfig {
    /// Mark this entry as system-locked — non-overridable by lower tiers.
    ///
    /// Called by the config loader on each entry of the system-scope file's
    /// (`/etc/ocx/config.toml`) `[registries]` table, after parsing and before
    /// folding higher tiers in. Unconditional: a `[registries.<name>]` entry
    /// has no opt-out field to gate on, mirroring
    /// [`MirrorConfig::lock_as_system`](crate::config::MirrorConfig::lock_as_system).
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    ///
    /// A system-locked entry (`self.system_locked`) ignores ALL lower-tier
    /// overrides. The locked flag stays on `self` (sticky). The loader folds
    /// the system tier in FIRST as the accumulator base, so `self` is the
    /// system entry when locked.
    pub fn merge(&mut self, other: RegistryConfig) {
        if self.system_locked {
            return;
        }
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
            system_locked: false,
        };
        let higher = RegistryConfig {
            url: None,
            system_locked: false,
        };
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
            system_locked: false,
        };
        let higher = RegistryConfig {
            url: Some("new.example".to_string()),
            system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(
            lower.url.as_deref(),
            Some("new.example"),
            "Some in higher should override lower's url"
        );
    }

    // ── RegistryConfig system lock (mirrors MirrorConfig, per registry entry) ──

    /// `lock_as_system` is unconditional — no opt-out field like
    /// `[patches].required` exists for a `[registries.<name>]` entry.
    #[test]
    fn registry_config_lock_as_system_sets_locked() {
        let mut registry = RegistryConfig {
            url: Some("ghcr.io".to_string()),
            system_locked: false,
        };
        registry.lock_as_system();
        assert!(registry.system_locked, "lock_as_system must set system_locked");
    }

    /// A system-locked `RegistryConfig` entry ignores a lower-tier override;
    /// the lock flag stays sticky after merge.
    #[test]
    fn registry_config_merge_system_locked_ignores_lower_tier() {
        let mut system = RegistryConfig {
            url: Some("system-registry.corp".to_string()),
            system_locked: false,
        };
        system.lock_as_system();
        assert!(system.system_locked);

        let user = RegistryConfig {
            url: Some("user-registry.corp".to_string()),
            system_locked: false,
        };
        system.merge(user);

        assert_eq!(
            system.url.as_deref(),
            Some("system-registry.corp"),
            "locked system registry entry must not be redirected by a lower tier"
        );
        assert!(system.system_locked, "lock flag stays sticky after merge");
    }
}
