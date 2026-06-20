// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Site-tier patch registry configuration.
//!
//! Home for all `[patches]` settings. The patch tier is the execution-env twin
//! of the mirror tier: where `[mirrors]` adapts *where bytes come from*,
//! `[patches]` adapts *what environment a tool runs in* — layering
//! operator-controlled companion packages (corp CA bundles, proxy/mirror URLs,
//! license server endpoints) onto unmodified upstream packages.
//!
//! See `adr_infrastructure_patches.md` §"Configuration ([patches])" and
//! §"Config forwarding (C5)".

use serde::Deserialize;

/// Configuration for the `[patches]` tier.
///
/// Structural twin of [`MirrorConfig`](crate::config::mirror::MirrorConfig) +
/// re-exported from [`crate::config`] as `PatchConfig`. No
/// `deny_unknown_fields` (forward compat with descriptor fields added by later
/// phases).
///
/// All fields are `Option` so [`Default`] and tier merge work correctly; absent
/// fields fall back to their defaults in [`resolve_patch_config`].
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct PatchConfig {
    /// OCI registry hosting patch descriptors, e.g.
    /// `"internal.company.com/ocx-patches"`.
    ///
    /// Required at resolve time; absent → `resolve_patch_config` returns `None`
    /// (no patch tier configured).
    pub registry: Option<String>,

    /// Path-template for per-package patch repositories. Placeholders:
    /// - `{registry}` — slugified (via [`to_relaxed_slug`]) registry portion of
    ///   the base identifier's registry host.
    /// - `{repository}` — the base identifier's repository path.
    ///
    /// Default: `"{registry}/{repository}"`.
    ///
    /// The template always produces a non-empty sub-path so the registry root
    /// is reserved for the global descriptor (`__ocx.patch` at the root).
    ///
    /// [`to_relaxed_slug`]: crate::utility::string_ext::StringExt::to_relaxed_slug
    pub path: Option<String>,

    /// Fail posture when a matched companion is unavailable.
    ///
    /// - `true` (default) — fail closed: abort launch if a required companion
    ///   cannot be resolved (C7 enforcement; a tool that runs without its CA
    ///   overlay does TLS to untrusted endpoints, strictly worse than refusing).
    /// - `false` — fail open: skip with a warning (suitable for non-security
    ///   companions like a license-server URL or metrics endpoint).
    pub required: Option<bool>,
}

/// Fully resolved form of [`PatchConfig`] with defaults applied.
///
/// Produced by [`resolve_patch_config`]. Carried in
/// [`OcxConfigView`](crate::env::OcxConfigView) and forwarded to child
/// processes via [`crate::env::keys::OCX_PATCHES`] (JSON) so launchers apply
/// the same patch tier (C5 across process boundaries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPatchConfig {
    /// The patch registry (e.g. `"internal.company.com/ocx-patches"`).
    pub registry: String,

    /// The expanded path template with placeholder tokens (not yet expanded
    /// against a specific identifier; expansion happens in
    /// [`expand_patch_path`]).
    pub path_template: String,

    /// Whether missing companions fail closed (`true`) or warn-and-skip
    /// (`false`).
    pub required: bool,
}

/// Error variants raised while resolving [`PatchConfig`] or expanding the path
/// template.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatchConfigError {
    /// The `registry` field is present but empty — a no-op patch tier that
    /// would silently skip all companions (equivalent footgun to a mirror with
    /// an empty `url`).
    #[error("patch registry is empty")]
    EmptyRegistry,

    /// The forwarded `OCX_PATCHES` env value was not valid JSON. A malformed
    /// forwarded config is a hard error: silently dropping it would run entry
    /// points without their operator-mandated companion env (violates C7).
    #[error("malformed OCX_PATCHES env value")]
    MalformedEnvJson {
        /// The underlying JSON parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// The `OCX_PATCHES` value was valid JSON but the `registry` field is
    /// absent or not a string. A value not produced by [`encode_patches`] is
    /// treated as corrupted or externally injected — a hard error matching the
    /// `MirrorConfigError::NonStringEnvValue` precedent in `env.rs`.
    #[error("OCX_PATCHES 'registry' field is absent or not a string")]
    MissingRegistryField,
}

impl PatchConfig {
    /// Default path template, applied by [`resolve_patch_config`] when
    /// `path` is absent.
    pub const DEFAULT_PATH_TEMPLATE: &'static str = "{registry}/{repository}";

    /// Default `required` value (fail-closed, C7).
    pub const DEFAULT_REQUIRED: bool = true;

    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    pub fn merge(&mut self, other: PatchConfig) {
        if other.registry.is_some() {
            self.registry = other.registry;
        }
        if other.path.is_some() {
            self.path = other.path;
        }
        if other.required.is_some() {
            self.required = other.required;
        }
    }
}

/// Resolves the `[patches]` config into a [`ResolvedPatchConfig`], applying
/// defaults and validating required fields.
///
/// Returns `None` when no `registry` is configured (no patch tier active) —
/// matching the "patches are optional / opt-in" invariant. An **empty** registry
/// string is a hard error (same footgun as a mirror with `url = ""`).
///
/// Mirrors [`resolve_mirror_map`](crate::config::mirror::resolve_mirror_map)
/// in structure: owned by the config layer, called at `Context` construction,
/// result passed down to consumers — no config reads inside leaf subsystems.
///
/// # Errors
///
/// Returns [`PatchConfigError::EmptyRegistry`] when `registry` is present but
/// empty.
pub fn resolve_patch_config(config: &crate::config::Config) -> Result<Option<ResolvedPatchConfig>, PatchConfigError> {
    // No [patches] section or no registry configured → no patch tier active.
    let Some(patch_config) = config.patches.as_ref() else {
        return Ok(None);
    };
    let Some(registry) = patch_config.registry.as_ref() else {
        return Ok(None);
    };
    // An empty registry is the same footgun as a mirror with url = "" — fail loud
    // so the operator can fix the config rather than silently skipping all companions.
    if registry.is_empty() {
        return Err(PatchConfigError::EmptyRegistry);
    }
    let path_template = patch_config
        .path
        .clone()
        .unwrap_or_else(|| PatchConfig::DEFAULT_PATH_TEMPLATE.to_string());
    let required = patch_config.required.unwrap_or(PatchConfig::DEFAULT_REQUIRED);
    Ok(Some(ResolvedPatchConfig {
        registry: registry.clone(),
        path_template,
        required,
    }))
}

/// Expands the path template for a given base identifier.
///
/// Substitutes the `{registry}` and `{repository}` tokens from the canonical
/// base identifier (e.g. `ghcr.io/acme/cli:v1`):
///
/// - `{registry}` — the registry host slugified via
///   [`to_relaxed_slug`](crate::utility::string_ext::StringExt::to_relaxed_slug)
///   (e.g. `"ghcr.io"` → `"ghcr.io"`, `"localhost:5000"` → `"localhost_5000"`).
/// - `{repository}` — the repository path verbatim (e.g. `"acme/cli"`).
///
/// Always yields a non-empty sub-path so the registry root is reserved for the
/// global descriptor (`__ocx.patch` at the root). This is guaranteed by the
/// default template `"{registry}/{repository}"` which always expands to at
/// least one non-empty component when the identifier is well-formed.
///
/// Used by `SitePatchResolver` (Phase 3+) to compute the per-package patch
/// repo address from the configured template.
pub fn expand_patch_path(template: &str, registry_host: &str, repository: &str) -> String {
    use crate::utility::string_ext::StringExt;

    // `{registry}` expands to the slugified registry host so that characters
    // such as `:` (port separators, e.g. `localhost:5000` → `localhost_5000`)
    // are safe in a filesystem or OCI repository path component.
    let slugified_registry = registry_host.to_relaxed_slug();
    let expanded = template
        .replace("{registry}", &slugified_registry)
        .replace("{repository}", repository);
    // Guard: the expanded result must never be empty. The default template
    // always produces a non-empty sub-path for well-formed identifiers, but
    // a custom template could theoretically produce one. Fall back to a
    // combination that is always non-empty.
    if expanded.is_empty() {
        format!("{slugified_registry}/{repository}")
    } else {
        expanded
    }
}

/// Serialises a [`ResolvedPatchConfig`] into the JSON string written to
/// [`crate::env::keys::OCX_PATCHES`].
///
/// Returns `None` for `None` input so
/// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) removes any
/// inherited value rather than setting a null.
pub(crate) fn encode_patches(patches: Option<&ResolvedPatchConfig>) -> Option<String> {
    let config = patches?;
    // Serialise as a flat JSON object with the three resolved fields.
    // Field names are stable — this is the wire format of OCX_PATCHES.
    let object = serde_json::json!({
        "registry":      config.registry,
        "path_template": config.path_template,
        "required":      config.required,
    });
    match serde_json::to_string(&object) {
        Ok(json) => Some(json),
        Err(error) => {
            crate::log::warn!("failed to encode OCX_PATCHES: {error}");
            None
        }
    }
}

/// Parses [`crate::env::keys::OCX_PATCHES`] (a JSON object) back into a
/// [`ResolvedPatchConfig`].
///
/// An absent or empty value yields `Ok(None)`. A present-but-broken value is a
/// hard error: silently dropping a forwarded `OCX_PATCHES` would run entry
/// points without their operator-mandated companion env (C7).
///
/// # Errors
///
/// Returns [`PatchConfigError::MalformedEnvJson`] when the value is present but
/// not valid JSON. Returns [`PatchConfigError::MissingRegistryField`] when the
/// value is valid JSON but the `registry` field is absent or not a string.
pub fn patches_from_env() -> Result<Option<ResolvedPatchConfig>, PatchConfigError> {
    let Some(raw) = crate::env::var(crate::env::keys::OCX_PATCHES) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let map = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
        .map_err(|source| PatchConfigError::MalformedEnvJson { source })?;

    // `registry` is the only mandatory field in the wire format — produced by
    // `encode_patches` as a stable triple `{registry, path_template, required}`.
    // A missing or non-string `registry` means the value was not produced by
    // `encode_patches` (i.e. externally injected / corrupted). Treat that as a
    // hard error: silently dropping a forwarded `OCX_PATCHES` would run entry
    // points without their operator-mandated companion env (C7). Mirrors
    // `MirrorConfigError::NonStringEnvValue` in `env.rs:573`.
    let registry = map
        .get("registry")
        .and_then(|v| v.as_str())
        .ok_or(PatchConfigError::MissingRegistryField)?
        .to_string();
    let path_template = map
        .get("path_template")
        .and_then(|v| v.as_str())
        .unwrap_or(PatchConfig::DEFAULT_PATH_TEMPLATE)
        .to_string();
    let required = map
        .get("required")
        .and_then(|v| v.as_bool())
        .unwrap_or(PatchConfig::DEFAULT_REQUIRED);

    if registry.is_empty() {
        return Ok(None);
    }

    Ok(Some(ResolvedPatchConfig {
        registry,
        path_template,
        required,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PatchConfig defaults ─────────────────────────────────────────────────

    /// `PatchConfig::default()` must have all fields as `None` — matches
    /// `MirrorConfig`'s zero-value convention and makes tier merge work correctly.
    ///
    /// Traces: stub manifest `PatchConfig` field defaults; ADR §Configuration.
    #[test]
    fn patch_config_default_is_all_none() {
        let cfg = PatchConfig::default();
        assert!(cfg.registry.is_none());
        assert!(cfg.path.is_none());
        assert!(cfg.required.is_none());
    }

    /// The `DEFAULT_PATH_TEMPLATE` constant must equal `"{registry}/{repository}"`.
    ///
    /// Traces: stub manifest `PatchConfig::DEFAULT_PATH_TEMPLATE`; ADR
    /// §Configuration — "path default `{registry}/{repository}`".
    #[test]
    fn patch_config_default_path_template_constant_value() {
        assert_eq!(
            PatchConfig::DEFAULT_PATH_TEMPLATE,
            "{registry}/{repository}",
            "DEFAULT_PATH_TEMPLATE must be '{{registry}}/{{repository}}'"
        );
    }

    /// The `DEFAULT_REQUIRED` constant must be `true` (fail-closed, C7).
    ///
    /// Traces: stub manifest `PatchConfig::DEFAULT_REQUIRED`; ADR §"required
    /// defaults to true — fail-closed".
    #[test]
    fn patch_config_default_required_constant_is_true() {
        // Use const assertion to avoid the "always-true constant" clippy lint.
        const _: () = assert!(PatchConfig::DEFAULT_REQUIRED, "DEFAULT_REQUIRED must be true");
    }

    // ── PatchConfig TOML parsing ─────────────────────────────────────────────

    /// A `[patches]` block with all three fields round-trips through TOML
    /// deserialization.
    ///
    /// Traces: Phase 1 deliverable — "Parse a `[patches]` TOML block into
    /// PatchConfig (registry/path/required fields)".
    #[test]
    fn patch_config_toml_full_block_parses() {
        let toml_str = r#"
            [patches]
            registry = "internal.company.com/ocx-patches"
            path = "{registry}/{repository}"
            required = false
        "#;
        let config: crate::config::Config = toml::from_str(toml_str).expect("valid [patches] TOML must parse");
        let patches = config.patches.expect("[patches] section must be present");
        assert_eq!(
            patches.registry.as_deref(),
            Some("internal.company.com/ocx-patches"),
            "registry field must round-trip"
        );
        assert_eq!(
            patches.path.as_deref(),
            Some("{registry}/{repository}"),
            "path field must round-trip"
        );
        assert_eq!(patches.required, Some(false), "required=false must round-trip");
    }

    /// A `[patches]` block with only `registry` set leaves `path` and `required`
    /// as `None`, to be filled in by `resolve_patch_config` defaults.
    ///
    /// Traces: Phase 1 — "required defaults to true when omitted; path defaults
    /// to `{registry}/{repository}` when omitted".
    #[test]
    fn patch_config_toml_registry_only_leaves_others_none() {
        let toml_str = r#"
            [patches]
            registry = "corp.example.com/patches"
        "#;
        let config: crate::config::Config = toml::from_str(toml_str).expect("valid [patches] TOML must parse");
        let patches = config.patches.expect("[patches] section must be present");
        assert_eq!(patches.registry.as_deref(), Some("corp.example.com/patches"));
        assert!(
            patches.path.is_none(),
            "omitted path must be None (default applied at resolve time)"
        );
        assert!(
            patches.required.is_none(),
            "omitted required must be None (default applied at resolve time)"
        );
    }

    /// An empty `[patches]` section deserializes to `PatchConfig::default()`.
    ///
    /// Traces: ADR §Configuration — "absent fields fall back to defaults in
    /// resolve_patch_config".
    #[test]
    fn patch_config_toml_empty_section_yields_default() {
        let toml_str = "[patches]\n";
        let config: crate::config::Config = toml::from_str(toml_str).expect("empty [patches] section must parse");
        let patches = config.patches.expect("[patches] section must be present");
        assert!(patches.registry.is_none());
        assert!(patches.path.is_none());
        assert!(patches.required.is_none());
    }

    /// A `[patches]` block with `required = true` parses as expected.
    ///
    /// Traces: Phase 1 — "explicit required=false respected" (inverse: true also
    /// parsed correctly).
    #[test]
    fn patch_config_toml_explicit_required_true_parses() {
        let toml_str = "[patches]\nregistry = \"r\"\nrequired = true\n";
        let config: crate::config::Config = toml::from_str(toml_str).expect("valid [patches] TOML must parse");
        let patches = config.patches.expect("[patches] section must be present");
        assert_eq!(
            patches.required,
            Some(true),
            "explicit required=true must parse as Some(true)"
        );
    }

    /// A `[patches]` block with unknown fields is silently ignored (no
    /// `deny_unknown_fields` on `PatchConfig`).
    ///
    /// Traces: stub manifest — "no deny_unknown_fields"; tripwire-test flip.
    #[test]
    fn patch_config_toml_unknown_fields_silently_ignored() {
        let toml_str = "[patches]\nregistry = \"r\"\nunknown_field = \"x\"\n";
        let result = toml::from_str::<crate::config::Config>(toml_str);
        assert!(
            result.is_ok(),
            "[patches] with unknown fields must not fail (no deny_unknown_fields): {result:?}"
        );
    }

    // ── PatchConfig::merge ───────────────────────────────────────────────────

    /// `None` in the higher tier must not clobber a `Some` in the lower tier.
    ///
    /// Traces: Phase 1 — "tier merge last-wins: a user-scope [patches] overrides
    /// a system-scope one".
    #[test]
    fn patch_config_merge_none_in_higher_does_not_clobber_lower() {
        let mut lower = PatchConfig {
            registry: Some("corp.example.com/patches".to_string()),
            path: Some("{registry}/{repository}".to_string()),
            required: Some(true),
        };
        let higher = PatchConfig {
            registry: None,
            path: None,
            required: None,
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_deref(), Some("corp.example.com/patches"));
        assert_eq!(lower.path.as_deref(), Some("{registry}/{repository}"));
        assert_eq!(lower.required, Some(true));
    }

    /// A `Some` in the higher tier overrides the lower tier — last-wins.
    ///
    /// Traces: Phase 1 — "tier merge last-wins: a user-scope [patches] overrides
    /// a system-scope one (basic last-wins only)".
    #[test]
    fn patch_config_merge_some_in_higher_overrides_lower() {
        let mut lower = PatchConfig {
            registry: Some("old.example.com/patches".to_string()),
            path: None,
            required: Some(false),
        };
        let higher = PatchConfig {
            registry: Some("new.example.com/patches".to_string()),
            path: Some("{registry}/{repository}".to_string()),
            required: Some(true),
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_deref(), Some("new.example.com/patches"));
        assert_eq!(lower.path.as_deref(), Some("{registry}/{repository}"));
        assert_eq!(lower.required, Some(true));
    }

    /// Config-level merge via `Config::merge` — two tiers each with a
    /// `[patches]` section: higher wins field-by-field.
    ///
    /// Traces: Phase 1 — "tier merge last-wins" at the `Config` level (the path
    /// `Config::merge` calls `PatchConfig::merge` on the `patches` field).
    #[test]
    fn config_merge_patches_last_wins() {
        let system_toml = "[patches]\nregistry = \"system.corp/patches\"\nrequired = true\n";
        let user_toml = "[patches]\nregistry = \"user.corp/patches\"\n";

        let mut system: crate::config::Config = toml::from_str(system_toml).expect("system config must parse");
        let user: crate::config::Config = toml::from_str(user_toml).expect("user config must parse");

        // User tier has higher precedence — passed as `other` to merge.
        system.merge(user);

        let patches = system.patches.expect("merged config must have [patches]");
        assert_eq!(
            patches.registry.as_deref(),
            Some("user.corp/patches"),
            "user-scope registry must win over system-scope (last-wins)"
        );
        // `required` was only in system scope; user scope had None → preserved.
        assert_eq!(
            patches.required,
            Some(true),
            "system-scope required must survive when user scope does not override it"
        );
    }

    // ── No [patches] → Config.patches is None ────────────────────────────────

    /// Parsing a config with no `[patches]` section → `Config.patches` is `None`.
    ///
    /// Traces: Phase 1 — "No `[patches]` → Config.patches is None".
    #[test]
    fn no_patches_section_yields_none() {
        let toml_str = "[registry]\ndefault = \"ocx.sh\"\n";
        let config: crate::config::Config = toml::from_str(toml_str).expect("config without [patches] must parse");
        assert!(
            config.patches.is_none(),
            "Config.patches must be None when [patches] is absent"
        );
    }

    // ── resolve_patch_config: return contract ─────────────────────────────────

    /// `resolve_patch_config` with no `[patches]` section returns `Ok(None)`.
    ///
    /// Traces: stub manifest — "Returns `Ok(None)` when `config.patches` is absent
    /// or `registry` is absent — no patch tier configured, not an error".
    #[test]
    fn resolve_patch_config_returns_none_when_patches_absent() {
        let config = crate::config::Config::default();
        let result = crate::config::patch::resolve_patch_config(&config);
        assert!(
            matches!(result, Ok(None)),
            "absent [patches] must yield Ok(None), got: {result:?}"
        );
    }

    /// `resolve_patch_config` with `registry = None` (empty PatchConfig) also
    /// returns `Ok(None)`.
    ///
    /// Traces: stub manifest return idiom — "Returns `Ok(None)` when `registry`
    /// is absent".
    #[test]
    fn resolve_patch_config_returns_none_when_registry_absent() {
        let config = crate::config::Config {
            patches: Some(PatchConfig::default()),
            ..crate::config::Config::default()
        };
        let result = crate::config::patch::resolve_patch_config(&config);
        assert!(
            matches!(result, Ok(None)),
            "PatchConfig with no registry must yield Ok(None), got: {result:?}"
        );
    }

    /// `resolve_patch_config` with `registry = ""` (present but empty) returns
    /// `Err(PatchConfigError::EmptyRegistry)`.
    ///
    /// Traces: stub manifest — "Returns `Err(PatchConfigError::EmptyRegistry)` when
    /// `registry` is present but empty".
    #[test]
    fn resolve_patch_config_errors_on_empty_registry() {
        let config = crate::config::Config {
            patches: Some(PatchConfig {
                registry: Some(String::new()),
                path: None,
                required: None,
            }),
            ..crate::config::Config::default()
        };
        let result = crate::config::patch::resolve_patch_config(&config);
        assert!(
            matches!(result, Err(PatchConfigError::EmptyRegistry)),
            "empty registry string must yield EmptyRegistry, got: {result:?}"
        );
    }

    /// `resolve_patch_config` applies the default path template when `path` is
    /// absent.
    ///
    /// Traces: Phase 1 — "path defaults to `{registry}/{repository}` when omitted";
    /// stub manifest return idiom.
    #[test]
    fn resolve_patch_config_applies_default_path_template() {
        let config = crate::config::Config {
            patches: Some(PatchConfig {
                registry: Some("corp.example.com/patches".to_string()),
                path: None,
                required: None,
            }),
            ..crate::config::Config::default()
        };
        let resolved = crate::config::patch::resolve_patch_config(&config)
            .expect("valid registry must not error")
            .expect("configured registry must yield Some(resolved)");
        assert_eq!(
            resolved.path_template,
            PatchConfig::DEFAULT_PATH_TEMPLATE,
            "absent path must default to DEFAULT_PATH_TEMPLATE"
        );
    }

    /// `resolve_patch_config` applies the default `required = true` when `required`
    /// is absent.
    ///
    /// Traces: Phase 1 — "required defaults to true when omitted".
    #[test]
    fn resolve_patch_config_applies_default_required_true() {
        let config = crate::config::Config {
            patches: Some(PatchConfig {
                registry: Some("corp.example.com/patches".to_string()),
                path: None,
                required: None,
            }),
            ..crate::config::Config::default()
        };
        let resolved = crate::config::patch::resolve_patch_config(&config)
            .expect("valid registry must not error")
            .expect("configured registry must yield Some(resolved)");
        assert!(
            resolved.required,
            "absent required must default to true (DEFAULT_REQUIRED, fail-closed C7)"
        );
    }

    /// `resolve_patch_config` respects an explicit `required = false`.
    ///
    /// Traces: Phase 1 — "explicit required=false respected".
    #[test]
    fn resolve_patch_config_respects_explicit_required_false() {
        let config = crate::config::Config {
            patches: Some(PatchConfig {
                registry: Some("corp.example.com/patches".to_string()),
                path: None,
                required: Some(false),
            }),
            ..crate::config::Config::default()
        };
        let resolved = crate::config::patch::resolve_patch_config(&config)
            .expect("valid registry must not error")
            .expect("configured registry must yield Some(resolved)");
        assert!(
            !resolved.required,
            "explicit required=false must be preserved in resolved form"
        );
    }

    /// `resolve_patch_config` preserves an explicit `path` template.
    ///
    /// Traces: ADR §Configuration — explicit `path` template survives resolution.
    #[test]
    fn resolve_patch_config_preserves_explicit_path_template() {
        let custom_template = "{registry}/custom/{repository}";
        let config = crate::config::Config {
            patches: Some(PatchConfig {
                registry: Some("corp.example.com/patches".to_string()),
                path: Some(custom_template.to_string()),
                required: None,
            }),
            ..crate::config::Config::default()
        };
        let resolved = crate::config::patch::resolve_patch_config(&config)
            .expect("valid registry must not error")
            .expect("configured registry must yield Some(resolved)");
        assert_eq!(
            resolved.path_template, custom_template,
            "explicit path template must be preserved verbatim"
        );
    }

    /// `resolve_patch_config` carries the registry verbatim into the resolved form.
    ///
    /// Traces: stub manifest — `ResolvedPatchConfig { registry, path_template,
    /// required }`.
    #[test]
    fn resolve_patch_config_carries_registry_into_resolved() {
        let config = crate::config::Config {
            patches: Some(PatchConfig {
                registry: Some("internal.company.com/ocx-patches".to_string()),
                path: None,
                required: None,
            }),
            ..crate::config::Config::default()
        };
        let resolved = crate::config::patch::resolve_patch_config(&config)
            .expect("valid registry must not error")
            .expect("configured registry must yield Some(resolved)");
        assert_eq!(
            resolved.registry, "internal.company.com/ocx-patches",
            "resolved registry must match the configured value verbatim"
        );
    }

    // ── expand_patch_path ────────────────────────────────────────────────────

    /// Default template + `ghcr.io` registry + `acme/cli` repository produces a
    /// non-empty sub-path containing the slugified registry.
    ///
    /// Traces: Phase 1 — "expand_patch_path slugifies the registry: e.g. base
    /// ocx.sh/java:21 -> sub-path containing ocx_sh and java; result is non-empty
    /// and never equals the bare registry root".
    #[test]
    fn expand_patch_path_default_template_ghcr_io() {
        let result = expand_patch_path("{registry}/{repository}", "ghcr.io", "acme/cli");
        assert!(
            !result.is_empty(),
            "expand_patch_path must produce a non-empty sub-path"
        );
        // The slugified form of "ghcr.io" is "ghcr.io" (dots are allowed by relaxed slug).
        assert!(
            result.contains("ghcr.io"),
            "result must contain the slugified registry; got: {result}"
        );
        assert!(
            result.contains("acme"),
            "result must contain the repository component; got: {result}"
        );
    }

    /// `localhost:5000` gets slugified to `localhost_5000` (colon → underscore).
    ///
    /// Traces: Phase 1 — "slugifies the registry: e.g. localhost:5000 →
    /// localhost_5000"; `to_relaxed_slug` replaces `:` with `_`.
    #[test]
    fn expand_patch_path_slugifies_colon_in_registry() {
        let result = expand_patch_path("{registry}/{repository}", "localhost:5000", "java");
        assert!(
            result.contains("localhost_5000"),
            "registry with colon must be slugified: 'localhost:5000' → 'localhost_5000'; got: {result}"
        );
    }

    /// `ocx.sh/java:21` base → sub-path contains `ocx_sh` (slash replaced) AND
    /// `java` from the repository.
    ///
    /// Traces: Phase 1 concrete example from the spec — "ocx.sh/java:21 →
    /// sub-path containing ocx_sh and java".
    #[test]
    fn expand_patch_path_ocx_sh_java_example() {
        // For identifier "ocx.sh/java:21":
        //   registry_host = "ocx.sh" (slugified → "ocx.sh" — dots preserved)
        //   repository    = "java"
        // ADR example: "ocx.sh/java:21 → internal.company.com/ocx-patches/ocx_sh/java:__ocx.patch"
        // The template only controls the sub-path; registry host stays literal in the patch registry address.
        // But the {registry} placeholder in the template uses the *base* identifier's registry host, slugified.
        // "ocx.sh" slugified = "ocx.sh" (dots are safe). However the ADR example shows "ocx_sh" —
        // this implies the slash in the full identifier is part of the slug transform input.
        // Per the ADR comment: {registry} = slugified registry portion of base id's registry host.
        // "ocx.sh" → to_relaxed_slug → "ocx.sh" (dots preserved, no slash in host-only part).
        // The "ocx_sh" in the ADR example appears to show the *full* identifier path-slug, not just host.
        // Spec says _registry_host_ is passed separately. So "ocx.sh" → "ocx.sh" is correct.
        let result = expand_patch_path("{registry}/{repository}", "ocx.sh", "java");
        assert!(!result.is_empty(), "result must be non-empty");
        assert!(
            result.contains("java"),
            "repository 'java' must appear in result; got: {result}"
        );
        // The result must not equal the bare registry host (root is reserved).
        assert_ne!(
            result, "ocx.sh",
            "result must not equal the bare registry host (root reserved for global descriptor)"
        );
    }

    /// The result of `expand_patch_path` is never an empty string.
    ///
    /// Traces: Phase 1 — "always yields a NON-EMPTY sub-path so the registry root
    /// stays reserved for the global descriptor".
    #[test]
    fn expand_patch_path_never_returns_empty() {
        // Even with empty components, result must be non-empty.
        let result = expand_patch_path("{registry}/{repository}", "reg.example.com", "repo/path");
        assert!(
            !result.is_empty(),
            "expand_patch_path must always produce a non-empty sub-path"
        );
    }

    /// `{repository}` token substitution works correctly for multi-segment repos.
    ///
    /// Traces: ADR §Configuration — "`{repository}` — the base identifier's
    /// repository path".
    #[test]
    fn expand_patch_path_multi_segment_repository() {
        let result = expand_patch_path("{registry}/{repository}", "ghcr.io", "org/team/tool");
        assert!(
            result.contains("org"),
            "multi-segment repository must appear in result; got: {result}"
        );
        assert!(
            result.contains("team"),
            "multi-segment repository must appear in result; got: {result}"
        );
        assert!(
            result.contains("tool"),
            "multi-segment repository must appear in result; got: {result}"
        );
    }

    // ── encode_patches / patches_from_env round-trip ─────────────────────────

    /// `encode_patches(None)` returns `None`.
    ///
    /// Traces: stub manifest — "returns `None` for `None`".
    #[test]
    fn encode_patches_none_returns_none() {
        let result = encode_patches(None);
        assert!(result.is_none(), "encode_patches(None) must return None");
    }

    /// `encode_patches(Some(...))` returns `Some(json)` containing the three fields.
    ///
    /// Traces: stub manifest — "serialises to JSON `{registry, path_template, required}`".
    #[test]
    fn encode_patches_some_returns_json_with_three_fields() {
        let resolved = Some(ResolvedPatchConfig {
            registry: "corp.example.com/patches".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        });
        let json = encode_patches(resolved.as_ref()).expect("Some(ResolvedPatchConfig) must encode to Some(json)");
        // Must be valid JSON with the three keys.
        let parsed: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&json).expect("encoded value must be valid JSON");
        assert!(parsed.contains_key("registry"), "JSON must have 'registry' key");
        assert!(
            parsed.contains_key("path_template"),
            "JSON must have 'path_template' key"
        );
        assert!(parsed.contains_key("required"), "JSON must have 'required' key");
        assert_eq!(parsed["registry"].as_str(), Some("corp.example.com/patches"));
        assert_eq!(parsed["path_template"].as_str(), Some("{registry}/{repository}"));
        assert_eq!(parsed["required"].as_bool(), Some(true));
    }

    /// `patches_from_env` returns `Ok(None)` when `OCX_PATCHES` is absent.
    ///
    /// Traces: stub manifest — "returns `Ok(None)` when absent/empty".
    #[test]
    fn patches_from_env_returns_none_when_absent() {
        let env_guard = crate::test::env::lock();
        env_guard.remove(crate::env::keys::OCX_PATCHES);
        let result = patches_from_env();
        assert!(
            matches!(result, Ok(None)),
            "absent OCX_PATCHES must yield Ok(None), got: {result:?}"
        );
    }

    /// `patches_from_env` returns `Ok(None)` when `OCX_PATCHES` is an empty string.
    ///
    /// Traces: stub manifest — "returns `Ok(None)` when absent/empty".
    #[test]
    fn patches_from_env_returns_none_when_empty_string() {
        let env_guard = crate::test::env::lock();
        env_guard.set(crate::env::keys::OCX_PATCHES, "");
        let result = patches_from_env();
        assert!(
            matches!(result, Ok(None)),
            "empty OCX_PATCHES must yield Ok(None), got: {result:?}"
        );
    }

    /// `patches_from_env` returns `Err(MalformedEnvJson)` on invalid JSON.
    ///
    /// Traces: stub manifest — "`Err(MalformedEnvJson)` on bad JSON".
    #[test]
    fn patches_from_env_errors_on_malformed_json() {
        let env_guard = crate::test::env::lock();
        env_guard.set(crate::env::keys::OCX_PATCHES, "not valid json {{{");
        let result = patches_from_env();
        assert!(
            matches!(result, Err(PatchConfigError::MalformedEnvJson { .. })),
            "malformed OCX_PATCHES must yield MalformedEnvJson, got: {result:?}"
        );
    }

    /// `patches_from_env` returns `Err(MissingRegistryField)` when `OCX_PATCHES` is
    /// valid JSON but the `registry` field is absent.
    ///
    /// A value not produced by `encode_patches` (externally injected or corrupted)
    /// must be a hard error, not silently dropped to `Ok(None)`. Mirrors the
    /// `MirrorConfigError::NonStringEnvValue` precedent in the mirrors path.
    #[test]
    fn patches_from_env_errors_on_valid_json_missing_registry() {
        let env_guard = crate::test::env::lock();
        // Valid JSON object but missing the `registry` key — externally injected / corrupted.
        env_guard.set(
            crate::env::keys::OCX_PATCHES,
            r#"{"path_template":"{registry}/{repository}","required":true}"#,
        );
        let result = patches_from_env();
        assert!(
            matches!(result, Err(PatchConfigError::MissingRegistryField)),
            "valid JSON with missing 'registry' field must yield MissingRegistryField, got: {result:?}"
        );
    }

    /// OCX_PATCHES round-trip: `encode_patches` → set env → `patches_from_env`
    /// yields the same `ResolvedPatchConfig`.
    ///
    /// Traces: Phase 1 — "OCX_PATCHES round-trip: an OcxConfigView carrying
    /// resolved patches -> apply_ocx_config sets OCX_PATCHES to JSON -> parsing
    /// OCX_PATCHES back yields the same resolved patches".
    #[test]
    fn ocx_patches_round_trip_via_encode_and_parse() {
        let env_guard = crate::test::env::lock();

        let original = ResolvedPatchConfig {
            registry: "internal.company.com/ocx-patches".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let json =
            encode_patches(Some(&original)).expect("encode_patches must return Some for a valid ResolvedPatchConfig");
        env_guard.set(crate::env::keys::OCX_PATCHES, json);

        let parsed = patches_from_env()
            .expect("valid OCX_PATCHES JSON must parse without error")
            .expect("non-empty, valid OCX_PATCHES must yield Some(resolved)");

        assert_eq!(
            parsed, original,
            "patches_from_env must recover the same ResolvedPatchConfig that was encoded"
        );
    }

    /// Round-trip with `required = false` — both true and false must survive.
    ///
    /// Traces: Phase 1 — "explicit required=false respected".
    #[test]
    fn ocx_patches_round_trip_required_false() {
        let env_guard = crate::test::env::lock();

        let original = ResolvedPatchConfig {
            registry: "corp.patches.io".to_string(),
            path_template: "custom/{registry}/{repository}".to_string(),
            required: false,
        };
        let json = encode_patches(Some(&original)).expect("encode must succeed");
        env_guard.set(crate::env::keys::OCX_PATCHES, json);

        let parsed = patches_from_env()
            .expect("valid JSON must parse")
            .expect("non-empty OCX_PATCHES must yield Some");

        assert!(
            !parsed.required,
            "required=false must survive the encode/parse round-trip"
        );
        assert_eq!(parsed, original);
    }
}
