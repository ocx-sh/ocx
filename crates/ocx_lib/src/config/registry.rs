// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-registry configuration.
//!
//! Home for all `[registries.<name>]` settings: since
//! `adr_index_indirection.md` F5a, the `index` field that selects the
//! resolution protocol for this namespace. Every `[registries.<name>]` key is
//! an identifier prefix, always — `[registry] default` takes a literal prefix
//! only, never a hostname alias (§6 ratified simplification). Future
//! per-registry fields (`insecure`, `location` rewrite, `timeout`, auth) land
//! here too, without forcing migration of existing configs.

use serde::Deserialize;

/// Configuration for a single `[registries.<name>]` entry.
///
/// Re-exported from [`crate::config`] as `RegistryConfig`. `deny_unknown_fields`
/// is enforced so typos inside a known section fail fast.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryConfig {
    /// Base URL of the `index.ocx.sh`-protocol index this namespace resolves
    /// through (`adr_index_indirection.md` F5a), e.g.
    /// `"https://index.ocx.sh"`. **Field presence is the kind marker**: a
    /// `[registries."<ns>"]` entry that carries `index` resolves via the
    /// ocx-index protocol (root → obs → `select_best`); an entry without
    /// `index` (or no entry at all) resolves as plain OCI. There is exactly
    /// one resolution protocol per namespace — no index-then-OCI-tags
    /// fallback chain, no runtime format probing.
    pub index: Option<String>,

    /// SSRF escape hatch for this namespace's physical registry hosts: exact
    /// hostnames or CIDR blocks whose resolved addresses skip the default-on
    /// private/loopback/link-local/metadata refusal.
    ///
    /// An index root's `repository` pointer arrives in remote-controlled data,
    /// so before OCX dereferences it into a physical fetch the target host must
    /// not resolve to a forbidden range (ocx#218). A private corporate registry
    /// legitimately lives on such a range, so listing it here (e.g.
    /// `["10.0.0.0/8", "registry.corp"]`) restores access for exactly that
    /// namespace without disabling the guard globally. The exemption is
    /// per-entry and managed-tier distributable, so `system_locked` applies —
    /// there is no CLI flag to widen a locked trust set.
    pub trusted_hosts: Option<Vec<String>>,

    /// Runtime provenance marker: this entry was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`), so it is NON-OVERRIDABLE by any lower
    /// tier for this registry name. Mirrors [`MirrorConfig`](crate::config::MirrorConfig)'s
    /// lock, but per-entry in the `[registries.<name>]` table.
    ///
    /// Never serialized — set by the loader via [`Self::lock_as_system`]
    /// after parsing the system-scope file, not read from disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,

    /// Runtime provenance marker: `index` here came from the compiled-in
    /// defaults tier (`ConfigLoader::builtin_defaults`), not from a config
    /// file. Cleared the moment any file tier restates `index` — including
    /// with the same value.
    ///
    /// Read by the CLI's `build_index_sources` to decide whether an explicit
    /// `[mirrors."<name>"]` entry suppresses the compiled-in index for this
    /// namespace. An operator who has already declared where this namespace's
    /// traffic goes must not silently gain a second host they never
    /// allow-listed; an `index` they wrote themselves still wins, because
    /// writing it clears this flag.
    ///
    /// Never serialized — set by the loader, not read from disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub index_is_compiled_default: bool,
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
    /// overrides, since `system_locked` is a per-entry lock, not a per-field
    /// one. The locked flag stays on `self` (sticky) and is ADOPTED from
    /// `other`: `Config::merge` reaches every `[registries.<name>]` entry
    /// through `map.entry(name).or_default().merge(..)`, so the system tier is
    /// never `self` on the fold that first carries it into the accumulator —
    /// without adopting the flag, the lock `apply_system_locks` set would be
    /// dropped on that very first fold and the entry would stay overridable by
    /// the user tier and by the untrusted managed-config payload.
    pub fn merge(&mut self, other: RegistryConfig) {
        if self.system_locked {
            return;
        }
        self.system_locked = other.system_locked;
        if other.index.is_some() {
            self.index = other.index;
            // Provenance travels with the value: a file tier restating `index`
            // takes ownership of it, even when the string is byte-identical to
            // the compiled-in default.
            self.index_is_compiled_default = other.index_is_compiled_default;
        }
        if other.trusted_hosts.is_some() {
            self.trusted_hosts = other.trusted_hosts;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 3.1.2: RegistryConfig::merge None-preserves ────────────────────
    // Plan: unit test gap — lower has Some(index), higher has None → lower preserved.

    #[test]
    fn registry_config_merge_none_in_higher_does_not_clobber_lower_index() {
        // Lower config has `index` set; higher config has None for the same field.
        // After merge, lower's `index` must be preserved (None never wins).
        let mut lower = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            ..Default::default()
        };
        let higher = RegistryConfig {
            index: None,
            ..Default::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.index.as_deref(),
            Some("https://index.ocx.sh"),
            "None in higher should not clobber lower's Some(index)"
        );
    }

    #[test]
    fn registry_config_merge_some_in_higher_overrides_lower_index() {
        // When higher has Some(index), it wins over lower's value.
        let mut lower = RegistryConfig {
            index: Some("https://old.example".to_string()),
            ..Default::default()
        };
        let higher = RegistryConfig {
            index: Some("https://new.example".to_string()),
            ..Default::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.index.as_deref(),
            Some("https://new.example"),
            "Some in higher should override lower's index"
        );
    }

    // ── Compiled-default provenance ──────────────────────────────────────────

    /// Writing `index` in a config file takes ownership of the value, so the
    /// compiled-default provenance does not survive — even when the string is
    /// byte-identical to the built-in one. That is what lets an operator who
    /// wants both a `[mirrors]` entry and the index path keep the index by
    /// naming it explicitly.
    #[test]
    fn a_declared_index_clears_the_compiled_default_provenance() {
        let mut builtin = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            index_is_compiled_default: true,
            ..Default::default()
        };
        builtin.merge(RegistryConfig {
            // Byte-identical to the built-in: only provenance distinguishes them.
            index: Some("https://index.ocx.sh".to_string()),
            ..Default::default()
        });
        assert!(
            !builtin.index_is_compiled_default,
            "a file tier restating `index` must clear the compiled-default marker"
        );
    }

    /// An entry that declares only `trusted_hosts` leaves `index` — and its
    /// provenance — alone, so the namespace stays suppressible by a mirror.
    #[test]
    fn an_entry_without_index_keeps_the_compiled_default_provenance() {
        let mut builtin = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            index_is_compiled_default: true,
            ..Default::default()
        };
        builtin.merge(RegistryConfig {
            trusted_hosts: Some(vec!["registry.corp".to_string()]),
            ..Default::default()
        });
        assert!(
            builtin.index_is_compiled_default,
            "an entry that never declares `index` must not claim ownership of it"
        );
    }

    // ── RegistryConfig system lock (mirrors MirrorConfig, per registry entry) ──

    /// `lock_as_system` is unconditional — no opt-out field like
    /// `[patches].required` exists for a `[registries.<name>]` entry.
    #[test]
    fn registry_config_lock_as_system_sets_locked() {
        let mut registry = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            ..Default::default()
        };
        registry.lock_as_system();
        assert!(registry.system_locked, "lock_as_system must set system_locked");
    }

    /// A system-locked `RegistryConfig` entry ignores a lower-tier override;
    /// the lock flag stays sticky after merge.
    #[test]
    fn registry_config_merge_system_locked_ignores_lower_tier() {
        let mut system = RegistryConfig {
            index: Some("https://system-index.corp".to_string()),
            ..Default::default()
        };
        system.lock_as_system();
        assert!(system.system_locked);

        let user = RegistryConfig {
            index: Some("https://user-index.corp".to_string()),
            ..Default::default()
        };
        system.merge(user);

        assert_eq!(
            system.index.as_deref(),
            Some("https://system-index.corp"),
            "locked system registry entry must not be redirected by a lower tier"
        );
        assert!(system.system_locked, "lock flag stays sticky after merge");
    }

    // ── `index` field TOML presence/absence (F5a) ────────────────────────────

    /// `[registries."ocx.sh"] index = "..."` parses into
    /// `RegistryConfig.index`.
    #[test]
    fn registries_table_parses_index_field_when_present() {
        let config: crate::config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://index.ocx.sh\"\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ocx.sh").expect("ocx.sh entry must exist");
        assert_eq!(entry.index.as_deref(), Some("https://index.ocx.sh"));
    }

    /// An entry declaring no fields at all leaves `index` absent.
    #[test]
    fn registries_table_index_field_absent_when_not_declared() {
        let config: crate::config::Config = toml::from_str("[registries.ghcr]\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ghcr").expect("ghcr entry must exist");
        assert!(
            entry.index.is_none(),
            "an entry declaring no fields must leave index absent"
        );
    }

    /// §6 ratified simplification: `RegistryConfig` no longer has a `url`
    /// field — `deny_unknown_fields` rejects it like any other typo, proving
    /// the alias is gone rather than merely unused.
    #[test]
    fn registries_table_rejects_removed_url_field() {
        let result: Result<crate::config::Config, _> = toml::from_str("[registries.ghcr]\nurl = \"ghcr.io\"\n");
        assert!(
            result.is_err(),
            "a 'url' field inside [registries.<name>] must be rejected — the field was removed (§6)"
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

    // ── `trusted_hosts` SSRF escape hatch (X2) ───────────────────────────────

    /// `trusted_hosts` parses a mixed list of hostnames and CIDR blocks — the
    /// per-entry SSRF escape hatch.
    #[test]
    fn registries_table_parses_trusted_hosts_list() {
        let config: crate::config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\ntrusted_hosts = [\"10.0.0.0/8\", \"registry.corp\"]\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ocx.sh").expect("ocx.sh entry must exist");
        assert_eq!(
            entry.trusted_hosts.as_deref(),
            Some(["10.0.0.0/8".to_string(), "registry.corp".to_string()].as_slice())
        );
    }

    /// A system-locked entry ignores a lower-tier `trusted_hosts` override — the
    /// whole-entry lock covers the SSRF escape hatch too, so a lower tier can
    /// never widen a locked trust set.
    #[test]
    fn registry_config_merge_system_locked_ignores_lower_tier_trusted_hosts() {
        let mut system = RegistryConfig {
            trusted_hosts: Some(vec!["10.0.0.0/8".to_string()]),
            ..Default::default()
        };
        system.lock_as_system();

        let user = RegistryConfig {
            trusted_hosts: Some(vec!["0.0.0.0/0".to_string()]),
            ..Default::default()
        };
        system.merge(user);

        assert_eq!(
            system.trusted_hosts.as_deref(),
            Some(["10.0.0.0/8".to_string()].as_slice()),
            "a locked entry's trust set must not be widened by a lower tier"
        );
    }

    /// `deny_unknown_fields` still rejects a typo of `trusted_hosts`.
    #[test]
    fn registries_table_rejects_typo_of_trusted_hosts_field() {
        let result: Result<crate::config::Config, _> =
            toml::from_str("[registries.\"ocx.sh\"]\ntrusted_host = [\"registry.corp\"]\n");
        assert!(
            result.is_err(),
            "a typo'd 'trusted_host' field must be rejected by deny_unknown_fields"
        );
    }
}
