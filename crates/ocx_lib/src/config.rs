// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;
pub mod loader;
pub mod managed;
pub mod mirror;
pub mod patch;
pub mod registry;

use std::collections::HashMap;

use serde::Deserialize;

pub use self::managed::ManagedConfig;
pub use self::mirror::MirrorConfig;
pub use self::patch::PatchConfig;
pub use self::registry::RegistryConfig;

/// Root configuration struct.
///
/// No `deny_unknown_fields` anywhere in this tree — unknown sections AND
/// unknown keys inside a known section are silently ignored. The reason is
/// fleet forward-compat, not convenience: one `config.toml` is read by many
/// ocx versions at once (the `[managed]` tier ships an operator's payload to
/// every binary in a fleet), so a file written for a newer ocx must degrade to
/// "the parts I understand" on an older one. The alternative — rejecting the
/// file — takes the WHOLE payload out of service on every older binary, which
/// is how a central rollout bricks a fleet.
///
/// The cost is that a typo'd key silently no-ops. When a change genuinely
/// cannot degrade (a key whose meaning or value shape changed), the escape
/// hatch is the tier's own OCI tag: publish the new payload under a new tag
/// and move fleets onto it as they upgrade — see the 2026-07-31 amendment in
/// `adr_managed_config_tier.md` and `website/src/docs/user-guide.md`
/// ("Rolling out an incompatible change").
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct Config {
    /// Global registry-subsystem settings (`[registry]` section).
    ///
    /// In v1 contains only `default`, but reserved for future global settings
    /// (timeout, retry policy, default-credential-provider, etc.).
    pub registry: Option<RegistryDefaults>,

    /// Named per-registry configuration tables (`[registries.<name>]`).
    ///
    /// The plural name is deliberate: it matches Cargo's convention and avoids
    /// a TOML collision with the singular `[registry]` global-settings section.
    ///
    /// Every key is an identifier prefix, always — `[registry] default`
    /// never dereferences through this table (§6 ratified simplification).
    /// Future extensions (per-registry insecure flag, location rewrite,
    /// timeout, auth) drop into the same entry struct without breaking
    /// existing configs.
    pub registries: Option<HashMap<String, RegistryConfig>>,

    /// Per-traffic-host mirrors (`[mirrors."<host>"]`).
    ///
    /// Maps a canonical upstream traffic host (e.g. `"ghcr.io"`,
    /// `"index.ocx.sh"`) to replacement endpoint(s) so OCX routes read
    /// traffic to a corporate mirror instead of the firewall-blocked origin.
    /// Each value is a union (`adr_index_indirection.md` F5b): a bare string
    /// rewrites both traffic roles for that host; a `{registry?, index?}`
    /// table splits per role — `registry` rewrites OCI distribution traffic
    /// only, `index` rewrites index-tree traffic only. Replace semantics: no
    /// origin fallback. The canonical identifier and content-addressed digest
    /// stay unchanged, so an `ocx.lock` produced behind the mirror remains
    /// valid with direct egress and vice versa.
    ///
    /// Deserialized via [`mirror::deserialize_mirrors_table`] (hand-rolled,
    /// value-first) rather than a plain derive, so a malformed entry raises a
    /// named per-host error instead of an opaque `#[serde(untagged)]`
    /// variant-mismatch.
    #[serde(default, deserialize_with = "mirror::deserialize_mirrors_table")]
    pub mirrors: Option<HashMap<String, MirrorConfig>>,

    /// Site-tier patch configuration (`[patches]`).
    ///
    /// Points at an operator-controlled patch registry that provides companion
    /// packages (CA bundles, proxy env vars, license-server endpoints) layered
    /// onto unmodified upstream packages at compose time. The patch tier is the
    /// execution-env twin of `[mirrors]`: `[mirrors]` adapts transport, patches
    /// adapt the execution environment.
    ///
    /// Absent → no patch tier configured (opt-in, not required).
    pub patches: Option<PatchConfig>,

    /// Corporate managed-configuration tier (`[managed]`).
    ///
    /// Points at an operator-controlled OCI artifact that supplies a plain
    /// `config.toml` payload — mirrors, patches pointer, default registry —
    /// synced into local state and merged above the user config every
    /// invocation. See `crate::managed_config` for the fetch/persist domain
    /// layer and `adr_managed_config_tier.md`.
    ///
    /// Absent → no managed tier configured (opt-in, seeded by
    /// `ocx self setup --managed-config`).
    pub managed: Option<ManagedConfig>,

    /// Identity-pinned verification policies (`[[trust.policy]]`).
    ///
    /// Unlike every other section, trust policies **array-append** across the
    /// `config.toml` tiers rather than replace (see [`Config::merge`]) — the
    /// operator (config.toml) trust set is the union of system/user/`$OCX_HOME`.
    /// At verify time this operator set takes precedence over the project
    /// `ocx.toml` (`crate::trust::resolve_tiered`). Consumed by
    /// `ocx package verify`. See `crate::trust` and `adr_trust_policy.md`.
    pub trust: Option<crate::trust::TrustConfig>,
}

/// Global registry-subsystem settings (`[registry]` section).
///
/// No `deny_unknown_fields` — unknown keys are ignored, like every other
/// config table (see [`Config`]).
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct RegistryDefaults {
    /// Default registry for bare identifiers (e.g. `cmake:3.28` expands to
    /// `<default>/cmake:3.28`). Overridden by the `OCX_DEFAULT_REGISTRY`
    /// environment variable.
    ///
    /// Always a literal prefix (e.g. `"ghcr.io"`, `"ocx.sh"`) — never
    /// resolved through the `[registries.<name>]` table (§6 ratified
    /// simplification).
    pub default: Option<String>,

    /// Runtime provenance marker: this tier was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`), so it is NON-OVERRIDABLE by any lower
    /// tier. Mirrors [`PatchConfig`]'s C7 lock, but unconditional — unlike
    /// `[patches].required`, `[registry]` has no opt-out field, so any
    /// system-scope declaration is authoritative by itself.
    ///
    /// Never serialized — set by the loader via [`Self::lock_as_system`]
    /// after parsing the system-scope file, not read from disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

impl Config {
    /// Merge `other` into `self`. `other` has higher precedence — its set
    /// fields override `self`'s.
    ///
    /// Scalars: `other` wins when present (`Some`).
    /// Tables: merged key-by-key.
    pub fn merge(&mut self, other: Config) {
        if let Some(other_registry) = other.registry {
            match self.registry.as_mut() {
                Some(self_registry) => self_registry.merge(other_registry),
                None => self.registry = Some(other_registry),
            }
        }
        if let Some(other_registries) = other.registries {
            let map = self.registries.get_or_insert_with(HashMap::new);
            for (name, entry) in other_registries {
                map.entry(name).or_default().merge(entry);
            }
        }
        if let Some(other_mirrors) = other.mirrors {
            let map = self.mirrors.get_or_insert_with(HashMap::new);
            for (host, entry) in other_mirrors {
                map.entry(host).or_default().merge(entry);
            }
        }
        if let Some(other_patches) = other.patches {
            match self.patches.as_mut() {
                Some(self_patches) => self_patches.merge(other_patches),
                None => self.patches = Some(other_patches),
            }
        }
        if let Some(other_managed) = other.managed {
            match self.managed.as_mut() {
                Some(self_managed) => self_managed.merge(other_managed),
                None => self.managed = Some(other_managed),
            }
        }
        // Trust policies APPEND across tiers (union), never replace: a
        // higher tier adds a more-specific scope or widens the ANY-of set
        // (rotation), it never masks a lower tier's pin.
        if let Some(other_trust) = other.trust {
            match self.trust.as_mut() {
                Some(self_trust) => self_trust.policy.extend(other_trust.policy),
                None => self.trust = Some(other_trust),
            }
        }
    }

    /// The declared trust policies (empty when no `[trust]` section is set).
    #[must_use]
    pub fn trust_policies(&self) -> &[crate::trust::TrustPolicy] {
        self.trust.as_ref().map_or(&[], |trust| trust.policy.as_slice())
    }

    /// Return [`RegistryDefaults::default`] as a literal prefix.
    ///
    /// `[registry] default` is always a literal identifier prefix (e.g.
    /// `"ghcr.io"`, `"ocx.sh"`) — it never dereferences through the
    /// `[registries.<name>]` table (§6 ratified simplification). Returns
    /// `None` only when no default is configured at all.
    #[must_use]
    pub fn resolved_default_registry(&self) -> Option<&str> {
        self.registry.as_ref()?.default.as_deref()
    }
}

impl RegistryDefaults {
    /// Mark this tier as system-locked — non-overridable by lower tiers.
    ///
    /// Called by the config loader on the system-scope file
    /// (`/etc/ocx/config.toml`) after parsing and before folding higher tiers
    /// in. Unconditional: unlike [`PatchConfig::lock_as_system`], `[registry]`
    /// has no opt-out field to gate on — a system-scope declaration of
    /// `[registry]` is always authoritative.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    ///
    /// A system-locked tier (`self.system_locked`) ignores ALL lower-tier
    /// overrides. The locked flag stays on `self` (sticky). The loader folds
    /// the system tier in FIRST as the accumulator base, so `self` is the
    /// system tier when locked.
    pub fn merge(&mut self, other: RegistryDefaults) {
        if self.system_locked {
            return;
        }
        if other.default.is_some() {
            self.default = other.default;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parsing tests (Step 3.1) ─────────────────────────────────────────────

    #[test]
    fn parse_minimal_config_sets_default_registry() {
        // Plan: Step 3.1 — Parse minimal config
        let config: Config = toml::from_str("[registry]\ndefault = \"ghcr.io\"").unwrap();
        let registry = config.registry.expect("registry section should be present");
        assert_eq!(registry.default.as_deref(), Some("ghcr.io"));
    }

    #[test]
    fn parse_empty_file_produces_default_config() {
        // Plan: Step 3.1 — Parse empty file → Config::default() with registry == None
        let config: Config = toml::from_str("").unwrap();
        assert!(config.registry.is_none());
    }

    #[test]
    fn parse_unknown_top_level_key_silently_ignored() {
        // Plan: Step 3.1 — Unknown top-level key [foo] → silently ignored
        // Config root has no deny_unknown_fields
        let result: Result<Config, _> = toml::from_str("[foo]\nbar = \"x\"");
        assert!(result.is_ok(), "unknown top-level key should not fail: {result:?}");
    }

    #[test]
    fn parse_registries_named_table() {
        // [registries.<name>] is a live v1 feature — parses into the
        // `registries` HashMap on Config, one RegistryEntry per key.
        let config: Config = toml::from_str(
            "[registries.ghcr]\nindex = \"https://ghcr.example\"\n\n[registries.company]\nindex = \"https://index.company.com\"",
        )
        .unwrap();
        let registries = config.registries.expect("registries table should be present");
        assert_eq!(registries.len(), 2);
        assert_eq!(registries["ghcr"].index.as_deref(), Some("https://ghcr.example"));
        assert_eq!(
            registries["company"].index.as_deref(),
            Some("https://index.company.com")
        );
    }

    /// An unknown key inside a `[registries.<name>]` entry is ignored, and the
    /// known keys of that same entry still take effect.
    ///
    /// Fleet forward-compat: one `config.toml` (notably a `[managed]` payload)
    /// is read by many ocx versions at once, so a per-registry field a newer
    /// ocx understands must degrade to "ignored" on an older one rather than
    /// take the whole file out of service.
    #[test]
    fn parse_unknown_field_inside_registries_entry_is_ignored() {
        let config: Config = toml::from_str("[registries.ghcr]\nindex = \"https://ghcr.example\"\nfoo = \"bar\"\n")
            .expect("an unknown field inside [registries.<name>] must not fail the parse");
        let registries = config.registries.expect("registries table must be present");
        assert_eq!(
            registries["ghcr"].index.as_deref(),
            Some("https://ghcr.example"),
            "the known field must still take effect alongside the ignored one"
        );
    }

    /// Replaces the former tripwire `parse_unknown_future_patches_section_silently_ignored`.
    /// `[patches]` is now a RECOGNIZED section — parses into `Config.patches`.
    ///
    /// Traces: Phase 1 — "flip the placeholder tripwire test to positive parse tests";
    /// impl map — "tripwire test → flip to positive parse/expansion tests".
    #[test]
    fn parse_patches_section_is_recognized() {
        let result: Result<Config, _> = toml::from_str("[patches]\nregistry = \"corp.example.com/patches\"\n");
        assert!(result.is_ok(), "[patches] section must parse successfully: {result:?}");
        let config = result.unwrap();
        let patches = config.patches.expect("[patches] must populate Config.patches");
        assert_eq!(
            patches.registry.as_deref(),
            Some("corp.example.com/patches"),
            "registry field must be populated from [patches] TOML"
        );
    }

    /// Unknown fields inside `[patches]` are silently ignored (no `deny_unknown_fields`).
    ///
    /// Traces: stub manifest — "no `deny_unknown_fields`"; Phase 1 forward-compat requirement.
    #[test]
    fn parse_patches_section_ignores_unknown_fields() {
        // PatchConfig has no deny_unknown_fields, so unknown keys inside [patches]
        // must be silently ignored (forward compat for future Phase 2+ fields).
        let result: Result<Config, _> = toml::from_str("[patches]\na = \"b\"\n");
        assert!(
            result.is_ok(),
            "[patches] with unknown fields must not fail (no deny_unknown_fields): {result:?}"
        );
    }

    /// The `[registry]` analog of
    /// [`parse_unknown_field_inside_registries_entry_is_ignored`]: an unknown
    /// key is ignored and `default` still takes effect.
    #[test]
    fn parse_unknown_field_inside_registry_is_ignored() {
        let config: Config = toml::from_str("[registry]\ndefault = \"ghcr.io\"\nfoo = \"bar\"\n")
            .expect("an unknown field inside [registry] must not fail the parse");
        assert_eq!(
            config.registry.expect("[registry] must be present").default.as_deref(),
            Some("ghcr.io"),
            "the known field must still take effect alongside the ignored one"
        );
    }

    /// The whole point, end to end: a payload written by a NEWER ocx — an
    /// unknown top-level section, an unknown key in every table an older ocx
    /// knows, and a `[mirrors]` entry declaring only a future role — parses,
    /// and every setting this binary understands survives.
    ///
    /// This is the shape a central `[managed]` rollout ships. Before the
    /// forward-compat posture, any one of these lines took the entire payload
    /// out of service on every older binary in the fleet.
    #[test]
    fn parse_payload_from_a_newer_ocx_keeps_every_known_setting() {
        let config: Config = toml::from_str(
            "[registry]\n\
             default = \"corp.example.com\"\n\
             timeout = 30\n\
             [registries.\"corp.example.com\"]\n\
             index = \"https://index.corp.example.com\"\n\
             insecure = true\n\
             [mirrors.\"ghcr.io\"]\n\
             registry = \"https://mirror.corp/ghcr\"\n\
             attest = \"https://mirror.corp/attest\"\n\
             [mirrors.\"quay.io\"]\n\
             attest = \"https://mirror.corp/attest\"\n\
             [toolchain]\n\
             channel = \"stable\"\n",
        )
        .expect("a payload from a newer ocx must parse");

        assert_eq!(
            config.registry.expect("[registry] must survive").default.as_deref(),
            Some("corp.example.com")
        );
        let registries = config.registries.expect("[registries] must survive");
        assert_eq!(
            registries["corp.example.com"].index.as_deref(),
            Some("https://index.corp.example.com")
        );
        let mirrors = config.mirrors.expect("[mirrors] must survive");
        assert_eq!(
            mirrors["ghcr.io"].registry.as_deref(),
            Some("https://mirror.corp/ghcr"),
            "a known role beside an unknown one must still apply"
        );
        assert!(
            !mirrors.contains_key("quay.io"),
            "an entry declaring only a future role contributes nothing, but must not fail the parse"
        );
    }

    #[test]
    fn parse_registry_default_with_unknown_top_level_section() {
        // Plan: Step 3.1 — [registry] default present alongside unknown top-level section
        let config: Config = toml::from_str("[registry]\ndefault = \"x\"\n[foo]\nbar = 1").unwrap();
        let registry = config.registry.expect("registry section should be present");
        assert_eq!(registry.default.as_deref(), Some("x"));
    }

    // ── Config::default() tests (Step 3.2) ──────────────────────────────────

    #[test]
    fn default_config_has_no_registry_section() {
        // Plan: Step 3.2 — Config::default() → registry == None
        let config = Config::default();
        assert!(config.registry.is_none());
    }

    // ── Config::merge() tests (Step 3.2) ────────────────────────────────────

    #[test]
    fn merge_higher_precedence_default_wins() {
        // Plan: Step 3.2 — lower has Some(default="a"), higher has Some(default="b") → "b"
        let mut lower = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("a".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("b".into()),
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_ref().and_then(|r| r.default.as_deref()), Some("b"));
    }

    #[test]
    fn merge_none_in_higher_does_not_clobber_lower() {
        // Plan: Step 3.2 — lower has Some(default="a"), higher has None → preserved as "a"
        let mut lower = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("a".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: None,
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("a"),
            "None in higher should not clobber lower's value"
        );
    }

    #[test]
    fn merge_higher_registry_section_into_lower_none() {
        // Plan: Step 3.2 — lower has None registry, higher has Some(default="b")
        let mut lower = Config::default();
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("b".into()),
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_ref().and_then(|r| r.default.as_deref()), Some("b"));
    }

    #[test]
    fn merge_both_have_registry_section_with_different_fields() {
        // Plan: Step 3.2 — both have Some(RegistryDefaults) merged field-by-field
        // lower has default="lower-default", higher has default=None
        // result: lower's default preserved since higher has None
        let mut lower = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("lower-default".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: None,
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("lower-default")
        );
    }

    // ── RegistryDefaults system lock (mirrors PatchConfig C7) ────────────────

    /// `lock_as_system` is unconditional — no opt-out field like
    /// `[patches].required` exists for `[registry]`.
    #[test]
    fn registry_defaults_lock_as_system_sets_locked() {
        let mut registry = RegistryDefaults {
            default: Some("corp.example.com".to_string()),
            system_locked: false,
        };
        registry.lock_as_system();
        assert!(registry.system_locked, "lock_as_system must set system_locked");
    }

    /// A system-locked `RegistryDefaults` ignores a lower-tier override; the
    /// lock flag stays sticky after merge.
    #[test]
    fn registry_defaults_merge_system_locked_ignores_lower_tier() {
        let mut system = RegistryDefaults {
            default: Some("system.corp".to_string()),
            system_locked: false,
        };
        system.lock_as_system();
        assert!(system.system_locked);

        let user = RegistryDefaults {
            default: Some("user.corp".to_string()),
            system_locked: false,
        };
        system.merge(user);

        assert_eq!(
            system.default.as_deref(),
            Some("system.corp"),
            "locked system default must not be redirected by a lower tier"
        );
        assert!(system.system_locked, "lock flag stays sticky after merge");
    }

    // ── [registries.<name>] merge + resolution tests ────────────────────────

    #[test]
    fn merge_registries_adds_new_entries_and_updates_existing() {
        // Keys unique to `lower` survive; keys unique to `higher` appear;
        // keys in both are field-merged with `higher` winning on conflicts.
        let mut lower: Config = toml::from_str(
            "[registries.ghcr]\nindex = \"https://ghcr.example\"\n\n[registries.company]\nindex = \"https://old.company.com\"",
        )
        .unwrap();
        let higher: Config = toml::from_str(
            "[registries.company]\nindex = \"https://new.company.com\"\n\n[registries.private]\nindex = \"https://priv.co\"",
        )
        .unwrap();
        lower.merge(higher);
        let registries = lower.registries.unwrap();
        assert_eq!(registries.len(), 3);
        assert_eq!(registries["ghcr"].index.as_deref(), Some("https://ghcr.example"));
        assert_eq!(registries["company"].index.as_deref(), Some("https://new.company.com"));
        assert_eq!(registries["private"].index.as_deref(), Some("https://priv.co"));
    }

    /// §6 ratified simplification: `[registry] default` is always a literal
    /// prefix — a matching `[registries.<name>]` entry never changes the
    /// resolved value (no more `url`-alias dereference).
    #[test]
    fn resolved_default_registry_returns_literal_name_even_with_matching_entry() {
        let config: Config = toml::from_str(
            "[registry]\ndefault = \"ghcr\"\n\n[registries.ghcr]\nindex = \"https://index.ghcr.example\"",
        )
        .unwrap();
        assert_eq!(config.resolved_default_registry(), Some("ghcr"));
    }

    #[test]
    fn resolved_default_registry_returns_literal_name_when_no_entry() {
        // [registry] default = "ocx.sh" with no matching [registries.ocx.sh]
        // → returns the literal name (the only supported behavior — §6).
        let config: Config = toml::from_str("[registry]\ndefault = \"ocx.sh\"").unwrap();
        assert_eq!(config.resolved_default_registry(), Some("ocx.sh"));
    }

    #[test]
    fn merge_trust_policies_appends_across_tiers() {
        // Lower tier pins one scope; higher tier adds another. Both survive —
        // trust policies pool (union), they do not replace like scalars do.
        let mut lower: Config =
            toml::from_str("[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nidentity = \"a\"\noidc_issuer = \"iss\"")
                .unwrap();
        let higher: Config =
            toml::from_str("[[trust.policy]]\nscope = \"ghcr.io/other/*\"\nidentity = \"b\"\noidc_issuer = \"iss\"")
                .unwrap();
        lower.merge(higher);
        assert_eq!(lower.trust_policies().len(), 2);
    }

    #[test]
    fn trust_policies_empty_when_absent() {
        assert!(Config::default().trust_policies().is_empty());
    }

    #[test]
    fn resolved_default_registry_returns_none_when_no_default() {
        let config = Config::default();
        assert_eq!(config.resolved_default_registry(), None);
    }

    /// §6 ratified simplification killed the CWE-15 indirection class
    /// entirely: `resolved_default_registry` never reads the `[registries]`
    /// table, so a locked `[registry] default` cannot be hijacked by an
    /// injected `[registries.<name>]` entry — there is no dereference left to
    /// exploit. Regression coverage for the removal, not the old guard.
    #[test]
    fn resolved_default_registry_locked_registry_ignores_injected_entry() {
        // System tier: [registry] default = "corp", locked. No [registries.corp]
        // in the system file.
        let mut system = Config {
            registry: Some(RegistryDefaults {
                default: Some("corp".to_string()),
                system_locked: false,
            }),
            ..Config::default()
        };
        system.registry.as_mut().unwrap().lock_as_system();

        // A lower tier injects a FRESH [registries.corp] entry — it must have
        // zero effect on the resolved default, locked or not.
        let injected: Config =
            toml::from_str("[registries.corp]\nindex = \"https://evil.attacker.example\"").expect("payload must parse");
        system.merge(injected);

        assert_eq!(
            system.resolved_default_registry(),
            Some("corp"),
            "the [registries.<name>] table must never affect the resolved literal default"
        );
    }

    /// Finding #15: one table-driven check that every lockable config section
    /// actually honors `system_locked` in its `merge` — a locked system tier
    /// must ignore a lower-tier override. Covers all five sections that carry
    /// the `lock_as_system` pattern (`[patches]`, `[managed]`, `[registry]`,
    /// each `[registries.<name>]` entry, each `[mirrors."<host>"]` entry) so a
    /// newly-added lockable section without merge wiring is caught here rather
    /// than in scattered per-struct tests.
    #[test]
    fn every_lockable_section_respects_system_lock() {
        // Each row: section name + a closure that builds a system-locked
        // instance, merges a lower-tier override into it, and returns whether
        // the locked value survived AND the lock flag stayed sticky.
        type LockCheck = (&'static str, fn() -> bool);
        let checks: &[LockCheck] = &[
            ("[patches]", || {
                let mut system = PatchConfig {
                    registry: Some("system.corp/patches".to_string()),
                    required: Some(true),
                    ..PatchConfig::default()
                };
                system.lock_as_system();
                system.merge(PatchConfig {
                    registry: Some("lower.evil/patches".to_string()),
                    required: Some(false),
                    ..PatchConfig::default()
                });
                system.system_locked && system.registry.as_deref() == Some("system.corp/patches")
            }),
            ("[managed]", || {
                let mut system = ManagedConfig {
                    source: Some("system.corp/ocx-config:user".to_string()),
                    required: Some(true),
                    ..ManagedConfig::default()
                };
                system.lock_as_system();
                system.merge(ManagedConfig {
                    source: Some("lower.evil/ocx-config:user".to_string()),
                    required: Some(false),
                    ..ManagedConfig::default()
                });
                system.system_locked && system.source.as_deref() == Some("system.corp/ocx-config:user")
            }),
            ("[registry]", || {
                let mut system = RegistryDefaults {
                    default: Some("system.corp".to_string()),
                    system_locked: false,
                };
                system.lock_as_system();
                system.merge(RegistryDefaults {
                    default: Some("lower.evil".to_string()),
                    system_locked: false,
                });
                system.system_locked && system.default.as_deref() == Some("system.corp")
            }),
            ("[registries.<name>]", || {
                let mut system = RegistryConfig {
                    index: Some("https://system-index.corp".to_string()),
                    ..Default::default()
                };
                system.lock_as_system();
                system.merge(RegistryConfig {
                    index: Some("https://lower.evil".to_string()),
                    ..Default::default()
                });
                system.system_locked && system.index.as_deref() == Some("https://system-index.corp")
            }),
            ("[mirrors.\"<host>\"]", || {
                let mut system = MirrorConfig {
                    registry: Some("https://system-mirror.corp/ghcr-remote".to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                };
                system.lock_as_system();
                system.merge(MirrorConfig {
                    registry: Some("https://lower.evil/ghcr-remote".to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                });
                system.registry_system_locked
                    && system.registry.as_deref() == Some("https://system-mirror.corp/ghcr-remote")
            }),
        ];

        for (section, check) in checks {
            assert!(
                check(),
                "{section} must honor system_locked in merge: a locked system tier ignored a lower-tier override \
                 OR dropped the lock flag"
            );
        }
    }
}
