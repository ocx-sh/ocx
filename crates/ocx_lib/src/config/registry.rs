// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-registry configuration.
//!
//! Home for all `[registries.<name>]` settings: a hostname alias (`url`) for
//! `[registry] default`, and — since `adr_index_indirection.md` F5a — the
//! `index` field that selects the resolution protocol for this namespace.
//! Future per-registry fields (`insecure`, `location` rewrite, `timeout`,
//! auth) land here too, without forcing migration of existing configs.

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

    /// Base URL of the `index.ocx.sh`-protocol index this namespace resolves
    /// through (`adr_index_indirection.md` F5a), e.g.
    /// `"https://index.ocx.sh"`. **Field presence is the kind marker**: a
    /// `[registries."<ns>"]` entry that carries `index` resolves via the
    /// ocx-index protocol (root → obs → `select_best`); an entry without
    /// `index` (or no entry at all) resolves as plain OCI. There is exactly
    /// one resolution protocol per namespace — no index-then-OCI-tags
    /// fallback chain, no runtime format probing. Independent of `url`: an
    /// entry may carry a hostname alias, an `index` URL, or both.
    pub index: Option<String>,

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
    /// overrides — `url` and `index` alike, since `system_locked` is a
    /// per-entry lock, not a per-field one (mirroring the pre-F5a contract).
    /// The locked flag stays on `self` (sticky). The loader folds the system
    /// tier in FIRST as the accumulator base, so `self` is the system entry
    /// when locked.
    pub fn merge(&mut self, other: RegistryConfig) {
        if self.system_locked {
            return;
        }
        if other.url.is_some() {
            self.url = other.url;
        }
        if other.index.is_some() {
            self.index = other.index;
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
            index: None,
            system_locked: false,
        };
        let higher = RegistryConfig {
            url: None,
            index: None,
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
            index: None,
            system_locked: false,
        };
        let higher = RegistryConfig {
            url: Some("new.example".to_string()),
            index: None,
            system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(
            lower.url.as_deref(),
            Some("new.example"),
            "Some in higher should override lower's url"
        );
    }

    /// `index` merges independently of `url` — a lower tier's `url`-only
    /// entry keeps its `url` when a higher tier adds only `index` (F5a:
    /// hostname alias and protocol selector are independent fields on one
    /// entry).
    #[test]
    fn registry_config_merge_index_field_independent_of_url() {
        let mut lower = RegistryConfig {
            url: Some("ghcr.io".to_string()),
            index: None,
            system_locked: false,
        };
        let higher = RegistryConfig {
            url: None,
            index: Some("https://index.ocx.sh".to_string()),
            system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(
            lower.url.as_deref(),
            Some("ghcr.io"),
            "url must survive a higher tier that only sets index"
        );
        assert_eq!(
            lower.index.as_deref(),
            Some("https://index.ocx.sh"),
            "index from the higher tier must win when lower had none"
        );
    }

    // ── RegistryConfig system lock (mirrors MirrorConfig, per registry entry) ──

    /// `lock_as_system` is unconditional — no opt-out field like
    /// `[patches].required` exists for a `[registries.<name>]` entry.
    #[test]
    fn registry_config_lock_as_system_sets_locked() {
        let mut registry = RegistryConfig {
            url: Some("ghcr.io".to_string()),
            index: None,
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
            index: None,
            system_locked: false,
        };
        system.lock_as_system();
        assert!(system.system_locked);

        let user = RegistryConfig {
            url: Some("user-registry.corp".to_string()),
            index: None,
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

    /// The lock is per-entry (not per-field, unlike `MirrorConfig`): a
    /// system-locked entry ignores a lower-tier override on `index` too, not
    /// just `url` — `system_locked` gates the whole entry.
    #[test]
    fn registry_config_merge_system_locked_ignores_lower_tier_index_field_too() {
        let mut system = RegistryConfig {
            url: Some("system-registry.corp".to_string()),
            index: Some("https://system-index.corp".to_string()),
            system_locked: false,
        };
        system.lock_as_system();

        let user = RegistryConfig {
            url: Some("user-registry.corp".to_string()),
            index: Some("https://user-index.corp".to_string()),
            system_locked: false,
        };
        system.merge(user);

        assert_eq!(
            system.url.as_deref(),
            Some("system-registry.corp"),
            "locked system registry entry must not be redirected by a lower tier"
        );
        assert_eq!(
            system.index.as_deref(),
            Some("https://system-index.corp"),
            "the locked entry's index field must also ignore a lower-tier override"
        );
    }

    // ── `index` field TOML presence/absence (F5a) ────────────────────────────

    /// `[registries."ocx.sh"] index = "..."` parses into
    /// `RegistryConfig.index`; an entry that only sets `index` leaves `url`
    /// unset.
    #[test]
    fn registries_table_parses_index_field_when_present() {
        let config: crate::config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://index.ocx.sh\"\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ocx.sh").expect("ocx.sh entry must exist");
        assert_eq!(entry.index.as_deref(), Some("https://index.ocx.sh"));
        assert!(entry.url.is_none(), "an index-only entry must leave url unset");
    }

    /// An entry that only sets `url` leaves `index` absent — presence of
    /// `index` is the sole protocol-kind marker (F5a); it is never inferred
    /// from `url`.
    #[test]
    fn registries_table_index_field_absent_when_not_declared() {
        let config: crate::config::Config = toml::from_str("[registries.ghcr]\nurl = \"ghcr.io\"\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ghcr").expect("ghcr entry must exist");
        assert!(
            entry.index.is_none(),
            "an entry declaring only url must leave index absent, not inferred"
        );
    }

    /// `deny_unknown_fields` still rejects a typo'd field name near `index`
    /// (e.g. `indx`) — the F5a addition does not loosen the existing
    /// typo-rejection contract.
    #[test]
    fn registries_table_rejects_typo_of_index_field() {
        let result: Result<crate::config::Config, _> =
            toml::from_str("[registries.\"ocx.sh\"]\nindx = \"https://index.ocx.sh\"\n");
        assert!(
            result.is_err(),
            "a typo'd 'indx' field inside [registries.<name>] must be rejected by deny_unknown_fields"
        );
    }
}
