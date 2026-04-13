// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;
pub mod loader;
pub mod registry;

use std::collections::HashMap;

use serde::Deserialize;

pub use self::registry::RegistryConfig;

/// Root configuration struct.
///
/// No `deny_unknown_fields` — unknown top-level sections are silently ignored
/// for forward compatibility (future sections like `[patches]`, `[clean]`,
/// `[toolchain]` should not break existing configs).
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Config {
    /// `[registry]` section — global registry-subsystem settings.
    ///
    /// In v1 contains only `default`, but reserved for future global settings
    /// (timeout, retry policy, default-credential-provider, etc.).
    pub registry: Option<RegistryDefaults>,

    /// `[registries.<name>]` named registry tables — per-registry settings.
    ///
    /// The plural name is deliberate: it matches Cargo's convention and avoids
    /// a TOML collision with the singular `[registry]` global-settings section.
    ///
    /// In v1 each entry only has a `url` field, giving `[registry] default`
    /// a lookup target. Future extensions (per-registry insecure flag,
    /// location rewrite, timeout, auth) drop into the same [`RegistryConfig`]
    /// struct without breaking existing configs.
    pub registries: Option<HashMap<String, RegistryConfig>>,
}

/// `[registry]` section — global registry-subsystem settings.
///
/// `deny_unknown_fields` is enforced here so typos inside a known section
/// fail fast. Forward compatibility is preserved at the root via the absence
/// of `deny_unknown_fields` on `Config`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDefaults {
    /// Default registry for bare identifiers (e.g., `cmake:3.28` →
    /// `<default>/cmake:3.28`). Overridden by `OCX_DEFAULT_REGISTRY` env var.
    ///
    /// May be either a literal hostname (`"ghcr.io"`) or the name of a
    /// `[registries.<name>]` entry — the latter is resolved to its `url`
    /// by [`Config::resolved_default_registry`].
    pub default: Option<String>,
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
    }

    /// Resolve [`RegistryDefaults::default`] through the `[registries.<name>]`
    /// lookup table.
    ///
    /// If `[registry] default = "name"` and `[registries.name] url = "host"`,
    /// returns `Some("host")`. If the name has no matching entry, returns
    /// the name as-is (treating it as a literal hostname — the v1 behavior).
    /// Returns `None` only when no default is configured at all.
    #[must_use]
    pub fn resolved_default_registry(&self) -> Option<&str> {
        let name = self.registry.as_ref()?.default.as_deref()?;
        if let Some(entries) = self.registries.as_ref()
            && let Some(entry) = entries.get(name)
            && let Some(url) = entry.url.as_deref()
        {
            return Some(url);
        }
        Some(name)
    }
}

impl RegistryDefaults {
    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    pub fn merge(&mut self, other: RegistryDefaults) {
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
            "[registries.ghcr]\nurl = \"ghcr.io\"\n\n[registries.company]\nurl = \"registry.company.com\"",
        )
        .unwrap();
        let registries = config.registries.expect("registries table should be present");
        assert_eq!(registries.len(), 2);
        assert_eq!(registries["ghcr"].url.as_deref(), Some("ghcr.io"));
        assert_eq!(registries["company"].url.as_deref(), Some("registry.company.com"));
    }

    #[test]
    fn parse_unknown_field_inside_registries_entry_is_rejected() {
        // deny_unknown_fields on RegistryEntry — typos inside a known section fail fast.
        let result: Result<Config, _> = toml::from_str("[registries.ghcr]\nfoo = \"bar\"");
        assert!(
            result.is_err(),
            "unknown field inside [registries.<name>] should fail due to deny_unknown_fields"
        );
    }

    #[test]
    fn parse_unknown_future_patches_section_silently_ignored() {
        // Plan: Step 3.1 — Unknown future section [patches] → silently ignored
        let result: Result<Config, _> = toml::from_str("[patches]\na = \"b\"");
        assert!(result.is_ok(), "unknown [patches] should not fail: {result:?}");
    }

    #[test]
    fn parse_unknown_field_inside_registry_is_rejected() {
        // Plan: Step 3.1 — Unknown field in [registry] → rejected by deny_unknown_fields
        let result: Result<Config, _> = toml::from_str("[registry]\nfoo = \"bar\"");
        assert!(
            result.is_err(),
            "unknown field inside [registry] should fail due to deny_unknown_fields"
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
                default: Some("a".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults {
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
                default: Some("a".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults { default: None }),
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
                default: Some("lower-default".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults { default: None }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("lower-default")
        );
    }

    // ── [registries.<name>] merge + resolution tests ────────────────────────

    #[test]
    fn merge_registries_adds_new_entries_and_updates_existing() {
        // Keys unique to `lower` survive; keys unique to `higher` appear;
        // keys in both are field-merged with `higher` winning on conflicts.
        let mut lower: Config =
            toml::from_str("[registries.ghcr]\nurl = \"ghcr.io\"\n\n[registries.company]\nurl = \"old.company.com\"")
                .unwrap();
        let higher: Config = toml::from_str(
            "[registries.company]\nurl = \"new.company.com\"\n\n[registries.private]\nurl = \"priv.co\"",
        )
        .unwrap();
        lower.merge(higher);
        let registries = lower.registries.unwrap();
        assert_eq!(registries.len(), 3);
        assert_eq!(registries["ghcr"].url.as_deref(), Some("ghcr.io"));
        assert_eq!(registries["company"].url.as_deref(), Some("new.company.com"));
        assert_eq!(registries["private"].url.as_deref(), Some("priv.co"));
    }

    #[test]
    fn resolved_default_registry_returns_url_from_named_entry() {
        // [registry] default = "ghcr" + [registries.ghcr] url = "ghcr.io"
        // → resolves to "ghcr.io".
        let config: Config =
            toml::from_str("[registry]\ndefault = \"ghcr\"\n\n[registries.ghcr]\nurl = \"ghcr.io\"").unwrap();
        assert_eq!(config.resolved_default_registry(), Some("ghcr.io"));
    }

    #[test]
    fn resolved_default_registry_falls_back_to_literal_when_no_entry() {
        // [registry] default = "ocx.sh" with no matching [registries.ocx.sh]
        // → returns the literal name (backwards-compat with bare hostnames).
        let config: Config = toml::from_str("[registry]\ndefault = \"ocx.sh\"").unwrap();
        assert_eq!(config.resolved_default_registry(), Some("ocx.sh"));
    }

    #[test]
    fn resolved_default_registry_returns_none_when_no_default() {
        let config = Config::default();
        assert_eq!(config.resolved_default_registry(), None);
    }

    #[test]
    fn resolved_default_registry_falls_back_when_entry_has_no_url() {
        // [registries.ghcr] exists but has no `url` field → fall back to the name.
        let config: Config = toml::from_str("[registry]\ndefault = \"ghcr\"\n\n[registries.ghcr]").unwrap();
        assert_eq!(config.resolved_default_registry(), Some("ghcr"));
    }
}
